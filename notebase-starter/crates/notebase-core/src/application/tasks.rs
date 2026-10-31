//! Owned jobs outlive an IPC waiter and always return their service to the App.
use super::*;

pub struct DataTask {
    service: Box<dyn crate::data_services::DataService>,
    index: Option<Arc<dyn IndexSession>>,
    command: String,
    payload: Value,
    generation: u64,
    exclusive: bool,
    stopped: bool,
    events: Option<NoteEvents>,
}
impl DataTask {
    pub fn run(&mut self) -> Result<Value> {
        let now = crate::index::now_ms();
        if self.command == "maintenance" && !self.service.maintenance_due(now)? {
            return Ok(json!({"created":false,"prunedVersions":self.service.prune_versions(now)?}));
        }
        if self.command == "background_sync" {
            let status = self.service.sync_status()?;
            if status["state"] == "unconfigured" || status["authRequired"] == true {
                return Ok(json!({"started":false}));
            }
        }
        let pause = self.command == "maintenance"
            || self.command == "backup_create"
            || (self.command == "backup_restore" && self.payload["verifyOnly"] != true);
        if pause {
            if let Some(index) = &self.index {
                self.stopped = true;
                index.suspend()?;
            }
        }
        match self.command.as_str() {
            "export_note" => self.service.export(&self.payload),
            "backup_create" => self.service.backup_create(&self.payload),
            "backup_restore" => self.service.backup_restore(&self.payload),
            "maintenance" => self.service.maintenance(now),
            "sync_now" | "background_sync" => {
                if let Some(events) = &self.events {
                    events("sync.state", json!({"state":"pending"}));
                }
                self.service.sync_now(&self.payload)
            }
            _ => Err(AppError::new("E_INTERNAL", "未知数据任务")),
        }
    }
}
impl App {
    pub fn request_data_task_cancel(&self) {
        if let Some(flag) = &self.cancel_task {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    pub fn data_task_active(&self) -> bool {
        self.busy_command.is_some()
    }

    /// Call under the application lock, then run the returned job outside it.
    pub fn begin_data_task(&mut self, command: &str, payload: &Value) -> Result<DataTask> {
        if !matches!(
            command,
            "export_note"
                | "backup_create"
                | "backup_restore"
                | "sync_now"
                | "maintenance"
                | "background_sync"
        ) {
            return Err(AppError::new("E_INTERNAL", "未知数据任务"));
        }
        if self.data_task_active() {
            return Err(self.busy_error());
        }
        let vault = self.notes()?.store.vault_path();
        let id = content_hash(vault.as_bytes())[7..23].to_owned();
        let exclusive = !(command == "export_note"
            && payload["preview"] == true
            && payload["scope"] != "vault");
        let mut service = if exclusive {
            self.data
                .take()
                .ok_or_else(|| AppError::new("E_INTERNAL", "数据保障端口尚未装配"))?
        } else {
            self.data_factory
                .as_ref()
                .ok_or_else(|| AppError::new("E_INTERNAL", "数据保障工厂尚未装配"))?(
                &vault, &id,
            )?
        };
        if exclusive {
            self.busy_command = Some(command.to_owned());
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            service.set_cancel_flag(flag.clone());
            self.cancel_task = Some(flag);
        }
        Ok(DataTask {
            service,
            index: self.index.clone(),
            command: command.into(),
            payload: payload.clone(),
            generation: self.vault_generation,
            exclusive,
            stopped: false,
            events: self.events.clone(),
        })
    }

    /// Must execute even when the request is cancelled or the worker panics.
    pub fn finish_data_task(&mut self, task: DataTask, mut result: Result<Value>) -> Result<Value> {
        if task.generation != self.vault_generation {
            return Err(AppError::new("E_CANCELLED", "笔记库已切换，忽略旧数据任务"));
        }
        if !task.exclusive {
            return result;
        }
        self.data = Some(task.service);
        self.busy_command = None;
        self.cancel_task = None;
        if let Some(service) = &mut self.data {
            service.set_cancel_flag(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        }
        if task.stopped {
            self.cancel_index();
            if let Some(notes) = &self.notes {
                let vault = notes.store.vault_path();
                let id = content_hash(vault.as_bytes())[7..23].to_owned();
                self.index = self
                    .index_factory
                    .as_ref()
                    .map(|factory| factory(&vault, &id));
            }
        }
        if task.command == "backup_restore" && task.payload["verifyOnly"] != true && result.is_ok()
        {
            if task.payload["mode"] == "original" {
                let vault = self.notes()?.store.vault_path();
                let id = content_hash(vault.as_bytes())[7..23].to_owned();
                match (self.factory)(&vault).and_then(|store| self.assemble_notes(store, &id)) {
                    Ok(notes) => {
                        self.notes = Some(notes);
                        self.vault_generation += 1;
                    }
                    Err(error) => result = Err(error),
                }
            }
            if let Some(events) = &self.events {
                events(
                    "note.changed",
                    json!({"kind":"backup.restored","source":"external"}),
                );
            }
        }
        if matches!(task.command.as_str(), "sync_now" | "background_sync") {
            if result.as_ref().is_ok_and(|value| value["started"] == false) {
                return result;
            }
            // A failed transfer may already have changed files. Always reconcile.
            if let Some(index) = &self.index {
                index.refresh();
            }
            if let Some(events) = &self.events {
                events("note.changed", json!({"source":"external","kind":"sync"}));
                match &result {
                    Ok(value) => {
                        events("sync.state", json!({"state":value["state"]}));
                        if value["conflicts"]
                            .as_array()
                            .is_some_and(|paths| !paths.is_empty())
                        {
                            events("conflict.detected", json!({"paths":value["conflicts"]}));
                        }
                    }
                    Err(error) => events(
                        "sync.state",
                        json!({"state":"dirty","lastError":error.code}),
                    ),
                }
            }
        }
        result
    }

    pub fn next_background_task(&mut self, now: u64, sync: bool) -> Result<Option<DataTask>> {
        if self.notes.is_none() || self.data.is_none() || self.data_task_active() {
            return Ok(None);
        }
        if sync {
            if now.saturating_sub(self.last_sync_attempt) < 300_000 {
                return Ok(None);
            }
            self.last_sync_attempt = now;
        }
        self.begin_data_task(
            if sync {
                "background_sync"
            } else {
                "maintenance"
            },
            &json!({}),
        )
        .map(Some)
    }

    pub(super) fn busy_error(&self) -> AppError {
        let sync = self
            .busy_command
            .as_deref()
            .is_some_and(|c| c.contains("sync"));
        let mut error = AppError::new(
            if sync { "E_SYNC_BUSY" } else { "E_CANCELLED" },
            "后台数据任务执行中，编辑内容保留，请完成后重试",
        );
        error.retryable = true;
        error
    }
}
