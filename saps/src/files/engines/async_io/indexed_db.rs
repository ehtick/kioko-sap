use crate::errors::file::FileError;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    IdbDatabase, IdbFactory, IdbObjectStore, IdbOpenDbRequest, IdbRequest, IdbTransactionMode,
    IdbVersionChangeEvent, WorkerGlobalScope,
};

/// The single object store that holds every file, keyed by its path string.
///
/// Both the file IO and folder IO impls open this same store, so it is named here rather
/// than repeated in each. The key is the file's path as a string; the value is the file's
/// full contents, also stored as a string.
pub(crate) const STORE: &str = "files";

/// Wraps a JavaScript error value into a `FileError::Io`, tagging it with the path it
/// happened on.
///
/// The IndexedDB API surfaces failures as opaque `JsValue`s, so every call funnels them
/// through here to get a single consistent `FileError` shape carrying the path and a
/// best-effort text of the error.
///
/// # Arguments
/// * `context`: The path (file or folder) the failing operation was acting on.
/// * `error`: The JavaScript error value returned by the browser.
///
/// # Returns
/// A `FileError::Io` holding the context path and the stringified error.
/// Public so that stores layered on top of this engine in other crates can reuse it
/// rather than re-deriving the same mapping.
pub fn js_error(context: &str, error: JsValue) -> FileError {
    let message = error
        .as_string()
        .or_else(|| error.dyn_ref::<js_sys::Object>().map(|object| object.to_string().into()))
        .unwrap_or_else(|| format!("{error:?}"));
    FileError::Io { path: context.to_string(), message }
}

/// Fetches the IndexedDB factory from whatever global scope the wasm module is running in.
///
/// `web_sys::window()` only exists on the main thread. Inside a worker (dedicated or shared —
/// the websocket client hosts this code in a `SharedWorker`) the global object is a
/// `WorkerGlobalScope` instead, which exposes its own `indexedDB`. This tries the window
/// first and falls back to the worker scope, so the same engine works in both contexts.
///
/// # Arguments
/// * `context`: The database name the caller is acting on, used to tag any error.
///
/// # Returns
/// The scope's `IdbFactory`, or a `FileError` if the scope is unrecognised or has no IndexedDB.
/// Public so that stores layered on top of this engine in other crates can reuse it
/// rather than re-deriving the same mapping.
pub fn indexed_db_factory(context: &str) -> Result<IdbFactory, FileError> {
    // On the main thread the global scope is a `Window`; in a worker it is a `WorkerGlobalScope`.
    let factory = match web_sys::window() {
        Some(window) => window.indexed_db(),
        None => js_sys::global()
            .dyn_into::<WorkerGlobalScope>()
            .map_err(|_| {
                js_error(context, JsValue::from_str("no window or worker scope available"))
            })?
            .indexed_db(),
    };
    factory
        .map_err(|error| js_error(context, error))?
        .ok_or_else(|| js_error(context, JsValue::from_str("indexedDB is unavailable")))
}

/// Bridges an IndexedDB request's `onsuccess`/`onerror` events into an awaitable future.
///
/// IndexedDB requests are not promises: they report completion by firing events. This wraps
/// a request in a `Promise` whose executor installs the two handlers, so the request can be
/// `await`ed like any other async call. On success it resolves with the request's result
/// value; on error it rejects with the request's error.
///
/// # Arguments
/// * `request`: The IndexedDB request to wait on.
///
/// # Returns
/// The request's result `JsValue` on success, or the request's error `JsValue` on failure.
/// Public so that stores layered on top of this engine in other crates can reuse it
/// rather than re-deriving the same mapping.
pub async fn await_request(request: &IdbRequest) -> Result<JsValue, JsValue> {
    let request = request.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let success_request = request.clone();
        let onsuccess = Closure::once_into_js(move || {
            let result = success_request.result().unwrap_or(JsValue::UNDEFINED);
            let _ = resolve.call1(&JsValue::NULL, &result);
        });
        request.set_onsuccess(Some(onsuccess.unchecked_ref()));

        let error_request = request.clone();
        let onerror = Closure::once_into_js(move || {
            let error = error_request
                .error()
                .ok()
                .flatten()
                .map(JsValue::from)
                .unwrap_or_else(|| JsValue::from_str("indexed db request failed"));
            let _ = reject.call1(&JsValue::NULL, &error);
        });
        request.set_onerror(Some(onerror.unchecked_ref()));
    });
    JsFuture::from(promise).await
}

/// Opens (or creates) the IndexedDB database `name` and ensures the files store exists.
///
/// The object store is created in the `onupgradeneeded` handler, which the browser fires the
/// first time a database is created (or when its version increases). By the time the open
/// request succeeds the store is guaranteed to be present.
///
/// # Arguments
/// * `name`: The database name to open.
///
/// # Returns
/// The open `IdbDatabase`, or a `FileError` if the browser has no IndexedDB or the open fails.
async fn open_database(name: &str) -> Result<IdbDatabase, FileError> {
    let factory = indexed_db_factory(name)?;
    let open_request = factory.open_with_u32(name, 1).map_err(|error| js_error(name, error))?;

    // Create the object store the first time the database is built.
    let on_upgrade =
        Closure::<dyn FnMut(IdbVersionChangeEvent)>::new(move |event: IdbVersionChangeEvent| {
            let Some(target) = event.target() else { return };
            let Ok(request) = target.dyn_into::<IdbOpenDbRequest>() else { return };
            let Ok(result) = request.result() else { return };
            let Ok(database) = result.dyn_into::<IdbDatabase>() else { return };
            if !database.object_store_names().contains(STORE) {
                let _ = database.create_object_store(STORE);
            }
        });
    open_request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));

    // Wait for the open to complete; the upgrade handler runs before success fires.
    await_request(open_request.as_ref()).await.map_err(|error| js_error(name, error))?;
    // The upgrade has run by now, so the handler is safe to drop.
    drop(on_upgrade);

    open_request
        .result()
        .map_err(|error| js_error(name, error))?
        .dyn_into::<IdbDatabase>()
        .map_err(|error| js_error(name, error))
}

/// Deletes the whole IndexedDB database `name` — the hard reset that survives page reloads.
///
/// Any open connections to the database should be dropped first, or the browser holds the
/// delete until they close. Deleting a database that does not exist succeeds (IndexedDB
/// semantics), so callers need no existence check.
///
/// # Arguments
/// * `name`: The database to delete.
///
/// # Returns
/// `Ok(())` once the database is gone, or a `FileError` if the browser has no IndexedDB or the
/// delete fails.
pub async fn delete_database(name: &str) -> Result<(), FileError> {
    let factory = indexed_db_factory(name)?;
    let request = factory.delete_database(name).map_err(|error| js_error(name, error))?;
    await_request(request.as_ref()).await.map_err(|error| js_error(name, error))?;
    Ok(())
}

/// Async file IO backed by the browser's IndexedDB.
///
/// This is the async sibling of the blocking engines: it keys files by their path exactly
/// like `KvBlockingIo`, but the store is an IndexedDB object store in the browser, so every
/// operation is asynchronous. It holds the open database handle; the file and folder
/// behaviour is implemented in the `files` and `folders` sibling modules.
///
/// Being built on browser JS objects, this type is single-threaded (`!Send`/`!Sync`), which
/// matches how it is used inside a wasm module.
pub struct IndexedDbIo {
    /// The open IndexedDB database that holds the files store.
    pub(crate) db: IdbDatabase,
}

impl IndexedDbIo {
    /// Opens (or creates) the IndexedDB database `name` ready for file access.
    ///
    /// # Arguments
    /// * `name`: The database name to open.
    ///
    /// # Returns
    /// A ready `IndexedDbIo`, or a `FileError` if the database could not be opened.
    pub async fn new(name: &str) -> Result<Self, FileError> {
        let db = open_database(name).await?;
        Ok(Self { db })
    }

    /// Opens the files object store inside a fresh transaction of the given mode.
    ///
    /// A new transaction is opened per call. Callers that need several operations to be
    /// atomic must issue all of that transaction's requests before awaiting any of them,
    /// because an IndexedDB transaction auto-commits once control returns to the event loop
    /// with no pending request.
    ///
    /// # Arguments
    /// * `mode`: Whether the transaction is read-only or read-write.
    ///
    /// # Returns
    /// The files object store, or a `FileError` if the transaction could not be opened.
    /// Public so that stores layered on top of this engine in other crates can reuse it
    /// rather than re-deriving the same mapping.
    pub fn object_store(
        &self,
        mode: IdbTransactionMode,
    ) -> Result<IdbObjectStore, FileError> {
        let transaction = self
            .db
            .transaction_with_str_and_mode(STORE, mode)
            .map_err(|error| js_error(STORE, error))?;
        transaction.object_store(STORE).map_err(|error| js_error(STORE, error))
    }

    /// Reads every key currently stored, as owned path strings.
    ///
    /// # Returns
    /// All stored keys, or a `FileError` if the read fails.
    /// Public so that stores layered on top of this engine in other crates can reuse it
    /// rather than re-deriving the same mapping.
    pub async fn all_keys(&self) -> Result<Vec<String>, FileError> {
        let store = self.object_store(IdbTransactionMode::Readonly)?;
        let request = store.get_all_keys().map_err(|error| js_error(STORE, error))?;
        let result = await_request(&request).await.map_err(|error| js_error(STORE, error))?;
        let array = js_sys::Array::from(&result);
        Ok((0..array.length()).filter_map(|index| array.get(index).as_string()).collect())
    }

    /// Reads every key/value pair currently stored, as owned strings.
    ///
    /// Both the keys and the values requests are issued on the same read transaction before
    /// awaiting, so they line up by index (IndexedDB returns both in key order).
    ///
    /// # Returns
    /// All stored `(key, value)` pairs, or a `FileError` if the read fails.
    pub(crate) async fn all_entries(&self) -> Result<Vec<(String, String)>, FileError> {
        let store = self.object_store(IdbTransactionMode::Readonly)?;
        let keys_request = store.get_all_keys().map_err(|error| js_error(STORE, error))?;
        let values_request = store.get_all().map_err(|error| js_error(STORE, error))?;
        let keys = js_sys::Array::from(
            &await_request(&keys_request).await.map_err(|error| js_error(STORE, error))?,
        );
        let values = js_sys::Array::from(
            &await_request(&values_request).await.map_err(|error| js_error(STORE, error))?,
        );
        let mut entries = Vec::new();
        for index in 0..keys.length() {
            let key = keys.get(index).as_string().unwrap_or_default();
            let value = values.get(index).as_string().unwrap_or_default();
            entries.push((key, value));
        }
        Ok(entries)
    }
}
