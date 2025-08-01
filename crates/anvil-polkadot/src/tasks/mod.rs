//! Task management support

#![allow(rustdoc::private_doc_tests)]

use crate::{shutdown::Shutdown, tasks::block_listener::BlockListener};
use alloy_network::{AnyHeader, AnyNetwork};
use alloy_primitives::B256;
use alloy_provider::Provider;
use alloy_rpc_types::anvil::Forking;
use futures::StreamExt;
use std::{fmt, future::Future};
use tokio::{runtime::Handle, task::JoinHandle};

pub mod block_listener;

/// A helper struct for managing additional tokio tasks.
#[derive(Clone)]
pub struct TaskManager {
    /// Tokio runtime handle that's used to spawn futures, See [tokio::runtime::Handle].
    tokio_handle: Handle,
    /// A receiver for the shutdown signal
    on_shutdown: Shutdown,
}

impl TaskManager {
    /// Creates a new instance of the task manager
    pub fn new(tokio_handle: Handle, on_shutdown: Shutdown) -> Self {
        Self { tokio_handle, on_shutdown }
    }

    /// Returns a receiver for the shutdown event
    pub fn on_shutdown(&self) -> Shutdown {
        self.on_shutdown.clone()
    }

    /// Spawns the given task.
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
        self.tokio_handle.spawn(task)
    }

    /// Spawns the blocking task.
    pub fn spawn_blocking(&self, task: impl Future<Output = ()> + Send + 'static) {
        let handle = self.tokio_handle.clone();
        self.tokio_handle.spawn_blocking(move || {
            handle.block_on(task);
        });
    }

    /// Spawns a new [`BlockListener`] task that listens for new blocks (poll-based) See also
    /// [`Provider::watch_blocks`] and executes the future the `task_factory` returns for the new
    /// block hash
    pub fn spawn_block_poll_listener<P, F, Fut>(&self, provider: P, task_factory: F)
    where
        P: Provider<AnyNetwork> + 'static,
        F: Fn(B256) -> Fut + Unpin + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let shutdown = self.on_shutdown.clone();
        self.spawn(async move {
            let blocks = provider
                .watch_blocks()
                .await
                .unwrap()
                .into_stream()
                .flat_map(futures::stream::iter);
            BlockListener::new(shutdown, blocks, task_factory).await;
        });
    }

    /// Spawns a new [`BlockListener`] task that listens for new blocks (via subscription) See also
    /// [`Provider::subscribe_blocks()`] and executes the future the `task_factory` returns for the
    /// new block hash
    pub fn spawn_block_subscription<P, F, Fut>(&self, provider: P, task_factory: F)
    where
        P: Provider<AnyNetwork> + 'static,
        F: Fn(alloy_rpc_types::Header<AnyHeader>) -> Fut + Unpin + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let shutdown = self.on_shutdown.clone();
        self.spawn(async move {
            let blocks = provider.subscribe_blocks().await.unwrap().into_stream();
            BlockListener::new(shutdown, blocks, task_factory).await;
        });
    }
}

impl fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskManager").finish_non_exhaustive()
    }
}
