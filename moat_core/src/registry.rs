use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Generic typed registry for named components.
/// Used for: agent strategies, tool handlers, LLM providers,
/// memory backends, scanners, skill loaders.
///
/// Pattern: OpenJarvis RegistryBase<T> with @register decorator.
/// Rust equivalent: typed key-value store with thread-safe access.
pub struct Registry<T: Send + Sync + 'static> {
    items: Mutex<HashMap<String, Arc<T>>>,
}

impl<T: Send + Sync + 'static> Registry<T> {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(HashMap::new()),
        }
    }

    /// Register an item under a named key.
    pub fn register(&self, key: impl Into<String>, item: T) {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.insert(key.into(), Arc::new(item));
    }

    /// Get a registered item by key.
    pub fn get(&self, key: &str) -> Option<Arc<T>> {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.get(key).cloned()
    }

    /// Check if a key is registered.
    pub fn has(&self, key: &str) -> bool {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.contains_key(key)
    }

    /// List all registered keys.
    pub fn keys(&self) -> Vec<String> {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.keys().cloned().collect()
    }

    /// Number of registered items.
    pub fn count(&self) -> usize {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.len()
    }

    /// Remove a registered item.
    pub fn unregister(&self, key: &str) -> bool {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.remove(key).is_some()
    }
}

impl<T: Send + Sync + 'static> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct TestTool {
        name: &'static str,
        version: u32,
    }

    #[test]
    fn test_registry_register_and_get() {
        let reg = Registry::new();
        reg.register("fs_read", TestTool { name: "file_reader", version: 1 });
        reg.register("fs_write", TestTool { name: "file_writer", version: 1 });

        let tool = reg.get("fs_read").expect("must find fs_read");
        assert_eq!(tool.name, "file_reader");
        assert_eq!(tool.version, 1);
        assert!(reg.has("fs_write"));
        assert!(!reg.has("nonexistent"));
    }

    #[test]
    fn test_registry_keys_and_count() {
        let reg = Registry::new();
        reg.register("a", TestTool { name: "A", version: 1 });
        reg.register("b", TestTool { name: "B", version: 2 });
        reg.register("c", TestTool { name: "C", version: 3 });

        assert_eq!(reg.count(), 3);
        let mut keys = reg.keys();
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_registry_unregister() {
        let reg = Registry::new();
        reg.register("temp", TestTool { name: "T", version: 0 });
        assert!(reg.has("temp"));
        assert!(reg.unregister("temp"));
        assert!(!reg.has("temp"));
        assert!(!reg.unregister("temp"));
    }
}
