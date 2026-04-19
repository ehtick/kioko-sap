use std::collections::HashMap;
use std::pin::Pin;                                                                                                           
use std::future::Future;
use std::sync::{LazyLock, RwLock};
use serde_json::Value;                                                                                                       
use crate::errors::saps::SapsError;


pub type TaskFnPtr = fn(Value) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>>;


pub struct BackgroundTaskEntry {
    pub name: &'static str,
    pub handler: TaskFnPtr,                                   
}


pub static TASK_REGISTRY: LazyLock<RwLock<HashMap<String, TaskFnPtr>>> = LazyLock::new(||{
    RwLock::new(HashMap::new())
});
