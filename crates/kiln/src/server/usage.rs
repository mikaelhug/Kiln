use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

const KEEP: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Sample {
    pub at: i64,
    pub cpu: f64,
    pub memory: f64,
    pub limit: Option<f64>,
}

#[derive(Default)]
pub struct Usage {
    builds: Mutex<HashMap<i64, VecDeque<Sample>>>,
}

impl Usage {
    pub fn record(&self, build: i64, sample: Sample) -> bool {
        let mut builds = self.builds.lock().expect("usage");
        let samples = builds.entry(build).or_default();
        if samples.back().is_some_and(|last| last.at >= sample.at) {
            return false;
        }
        samples.push_back(sample);
        while samples.len() > KEEP {
            samples.pop_front();
        }
        true
    }

    pub fn history(&self, build: i64) -> Vec<Sample> {
        self.builds.lock().expect("usage").get(&build).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    pub fn forget(&self, build: i64) {
        self.builds.lock().expect("usage").remove(&build);
    }
}
