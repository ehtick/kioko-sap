use crate::define_dal_transactions;
use super::model::ScheduledTask;
use uuid::Uuid;


define_dal_transactions!(
    InsertScheduledTask => insert_scheduled_task(task: ScheduledTask) -> bool,
    GetDueScheduledTasks => get_due_scheduled_task() -> Vec<ScheduledTask>,
    PostScheduledTask => post_scheduled_task(task: ScheduledTask) -> bool,
);



