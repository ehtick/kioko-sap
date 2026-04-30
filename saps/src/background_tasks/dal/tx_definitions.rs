use super::model::QueuedTask;
use crate::define_dal_transactions;
use uuid::Uuid;

define_dal_transactions!(
    InsertBackgroundTask => insert_background_task(task: QueuedTask) -> bool,
    GetNextBackgroundTask => get_next_background_task() -> Option<QueuedTask>,
    MarkBackgroundTaskAsCompleted => mark_background_task_as_completed(id: Uuid) -> bool,
    MarkBackgroundTaskAsExited => mark_background_task_as_exited(id: Uuid) -> bool,
    GetBackgroundTaskById => mark_background_task_by_id(id: Uuid) -> Option<QueuedTask>,
);
