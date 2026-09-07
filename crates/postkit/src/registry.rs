use crate::publisher::Publisher;
use crate::types::{Capability, Site};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct Registry {
    inner: HashMap<Site, Arc<dyn Publisher>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn register(&mut self, p: Arc<dyn Publisher>) {
        self.inner.insert(p.site().clone(), p);
    }

    pub fn get(&self, site: &Site) -> Option<Arc<dyn Publisher>> {
        self.inner.get(site).cloned()
    }

    pub fn sites(&self) -> impl Iterator<Item = &Site> {
        self.inner.keys()
    }

    pub fn capabilities_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (site, p) in &self.inner {
            let caps: Vec<&str> = p.capabilities().iter().map(|c| c.as_str()).collect();
            map.insert(site.as_str().into(), serde_json::json!(caps));
        }
        serde_json::Value::Object(map)
    }

    pub fn capabilities_for(&self, site: &Site) -> Option<Vec<Capability>> {
        self.inner.get(site).map(|p| p.capabilities().to_vec())
    }
}
