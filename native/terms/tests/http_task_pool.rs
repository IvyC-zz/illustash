#![allow(clippy::unwrap_used)]

#[path = "common/wait.rs"]
mod wait;

// 直接纳入 vendored 源码，让私有调度的回归随宿主现有锁文件和测试入口运行。
// 也可用 rustc --test 单独运行，不需要给 vendor 新增 Cargo 配置或锁文件。
mod task_pool {
    include!("../vendor/tiny_http/src/util/task_pool.rs");

    #[test]
    fn queued_connections_reserve_idle_workers() {
        use std::sync::mpsc;
        use std::time::Instant;

        let pool = TaskPool::new();
        let deadline = crate::wait::deadline();
        let mut queue = loop {
            let queue = pool.sharing.todo.lock().unwrap();
            if pool.sharing.waiting_tasks.load(Ordering::Acquire) == MIN_THREADS {
                break queue;
            }
            drop(queue);
            assert!(Instant::now() < deadline, "初始工作线程未进入等待状态");
            thread::yield_now();
        };

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut releases = Vec::new();
        // 持锁跨过整个突发：通知已发出，但所有空闲线程都无法重新拿锁。
        // 这正是 accept 连续提交连接、被唤醒的线程尚未恢复的合法调度。
        for _ in 0..MIN_THREADS {
            let (release_tx, release_rx) = mpsc::channel::<()>();
            releases.push(release_tx);
            let started = started_tx.clone();
            let done = done_tx.clone();
            pool.spawn_with_queue(
                Box::new(move || {
                    started.send(()).unwrap();
                    // 模拟 keep-alive 仍在等待后续输入；发送端析构也会解除等待。
                    let _ = release_rx.recv();
                    done.send(()).unwrap();
                }),
                &mut queue,
            );
        }
        assert_eq!(queue.len(), MIN_THREADS, "应先复用现有空闲线程");

        let (extra_tx, extra_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        releases.push(release_tx);
        pool.spawn_with_queue(
            Box::new(move || {
                extra_tx.send(()).unwrap();
                let _ = release_rx.recv();
                done_tx.send(()).unwrap();
            }),
            &mut queue,
        );
        drop(queue);
        for _ in 0..MIN_THREADS {
            started_rx.recv_timeout(crate::wait::limit()).unwrap();
        }

        // 四个已有任务仍未释放；第五个任务必须已经有独立的执行机会。
        let result = extra_rx.recv_timeout(crate::wait::limit());
        drop(releases);
        // 失败路径也释放所有长任务，并确认排队任务最终执行，排除任务本身失效。
        for _ in 0..=MIN_THREADS {
            done_rx.recv_timeout(crate::wait::limit()).unwrap();
        }
        if result.is_err() {
            extra_rx.recv_timeout(crate::wait::limit()).unwrap();
        }
        let (reused_tx, reused_rx) = mpsc::channel();
        pool.spawn(Box::new(move || reused_tx.send(()).unwrap()));
        reused_rx.recv_timeout(crate::wait::limit()).unwrap();
        assert!(result.is_ok(), "排队任务重复占用了同一空闲线程，后续连接被阻塞: {result:?}");
    }
}
