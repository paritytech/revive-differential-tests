use std::sync::{
    LazyLock, Mutex,
    mpsc::{Receiver, Sender},
};

pub trait NodePool<T: Node> {
    fn access() -> &'static LazyLock<Mutex<Vec<T>>>;
}

use revive_dt_config::Arguments;

use crate::Node;

//static POOL: LazyLock<Mutex<Pool<T>>> = LazyLock::new(Default::default);

pub struct Handle<T> {
    node: T,
    notifier: Sender<()>,
}

pub struct Pool<T> {
    request: Receiver<()>,
    nodes: usize,
    handles: Vec<T>,
}

impl<T> Pool<T>
where
    T: Node,
{
    pub fn spawn() {}
}

// spawner: loops on a queue

pub fn get_handle<T: Node + NodePool<T>>(config: &Arguments) -> Receiver<T> {
    todo!()
}
