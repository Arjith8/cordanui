use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cordanui_plugin_runtime::ui::{BuffersHost, PanelSpec};

/// Host for `cord.buffers` — plugin-controlled buffers that appear as sheet tabs
/// but render a declarative PanelSpec instead of goals. Think Claude Code / Codex
/// model pickers: a chat plugin can `create_buffer{name, draw, on_key}` and it
/// shows up alongside "All" and sheet tabs.
pub struct BufferManager {
    buffers: Arc<Mutex<HashMap<String, PanelSpec>>>,
    active: Arc<Mutex<Option<String>>>,
}

impl BufferManager {
    pub fn new(
        buffers: Arc<Mutex<HashMap<String, PanelSpec>>>,
        active: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self { buffers, active }
    }
}

impl BuffersHost for BufferManager {
    fn create_buffer(&self, name: String, spec: PanelSpec) -> String {
        let id = format!("buffer:{}", name);
        self.buffers.lock().unwrap().insert(id.clone(), spec);
        id
    }

    fn update_buffer(&self, id: &str, spec: PanelSpec) -> anyhow::Result<()> {
        let mut map = self.buffers.lock().unwrap();
        if map.contains_key(id) {
            map.insert(id.to_string(), spec);
            Ok(())
        } else {
            anyhow::bail!("buffer not found: {}", id)
        }
    }

    fn remove_buffer(&self, id: &str) {
        self.buffers.lock().unwrap().remove(id);
        let mut active = self.active.lock().unwrap();
        if active.as_deref() == Some(id) {
            *active = None;
        }
    }

    fn select_buffer(&self, id: Option<String>) {
        *self.active.lock().unwrap() = id;
    }

    fn list_buffers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.buffers.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    fn current_buffer(&self) -> Option<String> {
        self.active.lock().unwrap().clone()
    }
}
