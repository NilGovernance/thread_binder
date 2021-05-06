extern crate hwloc2;
extern crate libc;
extern crate rayon;
use hwloc2::{ObjectType, Topology, TopologyObject, CpuBindFlags};
use std::sync::{
    Arc, Mutex,
};

/// Same as rayon's ThreadPoolBuilder expect you get an extra `bind` method.
pub struct ThreadPoolBuilder {
    builder: rayon::ThreadPoolBuilder,
    bind_policy: Policy,
}

impl Default for ThreadPoolBuilder {
    fn default() -> Self {
        ThreadPoolBuilder {
            builder: Default::default(),
            bind_policy: Policy::RoundRobinNuma,
        }
    }
}

/// This enum specifies how to dispatch threads on the machine.
pub enum Policy {
    /// Binds all threads in one numa node (1 thread per core until we run out of them).
    RoundRobinNuma,
    /// Do not bind.
    NoBinding,
}

impl ThreadPoolBuilder {
    /// Creates a new ThreadPoolBuilder. We bind to numa by default.
    pub fn new() -> Self {
        let topo = Arc::new(Mutex::new(Topology::new().unwrap()));
        ThreadPoolBuilder {
            builder: rayon::ThreadPoolBuilder::new().start_handler(move |thread_id| {
                bind_numa(thread_id, &topo);
            }),
            bind_policy: Policy::RoundRobinNuma,
        }
    }

    pub fn new_with_core_set(core_set: Vec<usize>) -> Self {
        let topo = Arc::new(Mutex::new(Topology::new().unwrap()));
        let core_set = Arc::new(core_set);
        ThreadPoolBuilder {
            builder: rayon::ThreadPoolBuilder::new().start_handler(move |thread_id| {
                bind_to_set(thread_id, &core_set, &topo);
            }),
            bind_policy: Policy::NoBinding,
        }
    }

    /// Set binding policy.
    pub fn bind(mut self, bind_policy: Policy) -> Self {
        self.bind_policy = bind_policy;
        self
    }

    pub fn start_handler<H>(mut self, start_handler: H) -> Self
    where
        H: Fn(usize) + Send + Sync + 'static,
    {
        let topo = Arc::new(Mutex::new(Topology::new().unwrap()));
        self.builder = self.builder.start_handler(move |thread_id| {
            bind_numa(thread_id, &topo);
            start_handler(thread_id);
        });
        self
    }

    /// Set number of threads wanted.
    pub fn num_threads(mut self, num_threads: usize) -> Self {
        self.builder = self.builder.num_threads(num_threads);
        self
    }

    /// Build the `ThreadPool`.
    pub fn build(self) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
        let pool = match self.bind_policy {
            Policy::RoundRobinNuma => self.builder.build(),
            Policy::NoBinding => self.builder.build(),
        };
        pool
    }

    /// Build the global `ThreadPool`.
    pub fn build_global(self) -> Result<(), rayon::ThreadPoolBuildError> {
        let topo = Arc::new(Mutex::new(Topology::new().unwrap()));

        match self.bind_policy {
            Policy::RoundRobinNuma => self
                .builder
                .start_handler(move |thread_id| {
                    bind_numa(thread_id, &topo);
                })
                .build_global(),
            Policy::NoBinding => self.builder.build_global(),
        }
    }
}

/// return if given ancestor is one of object's
fn has_ancestor(object: &TopologyObject, ancestor: &TopologyObject) -> bool {
    let father = object.parent();
    father
        .map(|f| {
            (f.object_type() == ancestor.object_type()
                && f.logical_index() == ancestor.logical_index())
                || has_ancestor(f, ancestor)
        })
        .unwrap_or(false)
}

fn bind_numa(thread_id: usize, topo: &Arc<Mutex<Topology>>) {
    let pthread_id = unsafe { libc::pthread_self() };
    let mut locked_topo = topo.lock().unwrap();
    let cpu_set = {
        // let's select one numa node (or above if none)
        let ancestor_level = locked_topo
            .depth_or_above_for_type(&ObjectType::NUMANode)
            .unwrap();
        let targets = locked_topo.objects_at_depth(ancestor_level);
        let ancestor = targets.first().expect("no common ancestor");
        // ok now look at all its processing units and take the one matching our thread_id
        let processing_units = locked_topo.objects_with_type(&ObjectType::PU).unwrap();
        let unit = processing_units
            .iter()
            .filter(|pu| has_ancestor(pu, ancestor))
            .cycle()
            .nth(thread_id)
            .expect("no cores below given ancestor");
        unit.cpuset().unwrap()
    };

    locked_topo
        .set_cpubind_for_thread(pthread_id, cpu_set, CpuBindFlags::CPUBIND_THREAD)
        .unwrap();
}

fn bind_to_set(thread_id: usize, core_set: &Arc<Vec<usize>>, topo: &Arc<Mutex<Topology>>) {
    let pthread_id = unsafe { libc::pthread_self() };
    let mut locked_topo = topo.lock().unwrap();
    let cpu_set = {
        let all_cores = (*locked_topo)
            .objects_with_type(&ObjectType::Core)
            .unwrap();
        //let res = Vec::with_capacity(core_set.len());
        let unit = all_cores
            .iter()
            .enumerate()
            .filter(|(idx, _core)| core_set.contains(idx))
            .map(|(_idx, core)| core)
            .cycle()
            .nth(thread_id)
            .unwrap();
        unit.cpuset().unwrap()
    };

    locked_topo
        .set_cpubind_for_thread(pthread_id, cpu_set, CpuBindFlags::CPUBIND_THREAD)
        .unwrap();
}