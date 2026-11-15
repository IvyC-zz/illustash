use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// 管理工作线程：现有线程全部被占用时新增线程，多余的空闲线程在等待数秒后退出。
pub struct TaskPool {
    sharing: Arc<Sharing>,
}

struct Sharing {
    // 等待工作线程领取的任务。
    todo: Mutex<VecDeque<Box<dyn FnMut() + Send>>>,

    // 向 todo 加入任务时通知等待线程。
    condvar: Condvar,

    // 当前工作线程总数。
    active_tasks: AtomicUsize,

    // 等待中的工作线程数，包含已唤醒但还未重新拿到 todo 锁的线程。
    waiting_tasks: AtomicUsize,
}

/// 保留的最少工作线程数。
static MIN_THREADS: usize = 4;

struct Registration<'a> {
    nb: &'a AtomicUsize,
}

impl<'a> Registration<'a> {
    fn new(nb: &'a AtomicUsize) -> Registration<'a> {
        nb.fetch_add(1, Ordering::Release);
        Registration { nb }
    }
}

impl<'a> Drop for Registration<'a> {
    fn drop(&mut self) {
        self.nb.fetch_sub(1, Ordering::Release);
    }
}

impl TaskPool {
    pub fn new() -> TaskPool {
        let pool = TaskPool {
            sharing: Arc::new(Sharing {
                todo: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                active_tasks: AtomicUsize::new(0),
                waiting_tasks: AtomicUsize::new(0),
            }),
        };

        for _ in 0..MIN_THREADS {
            pool.add_thread(None)
        }

        pool
    }

    /// 在线程中执行函数；没有可用线程时新增线程。
    pub fn spawn(&self, code: Box<dyn FnMut() + Send>) {
        let mut queue = self.sharing.todo.lock().expect("任务队列锁中毒");

        self.spawn_with_queue(code, &mut queue);
    }

    // 调用者持有 todo 锁；工作线程恢复后也必须先拿到同一把锁。
    fn spawn_with_queue(&self, code: Box<dyn FnMut() + Send>, queue: &mut VecDeque<Box<dyn FnMut() + Send>>) {
        // 每个排队任务已预订一个等待线程，不能把尚未恢复的线程重复分配给新任务。
        if queue.len() >= self.sharing.waiting_tasks.load(Ordering::Acquire) {
            self.add_thread(Some(code));
        } else {
            queue.push_back(code);
            self.sharing.condvar.notify_one();
        }
    }

    fn add_thread(&self, initial_fn: Option<Box<dyn FnMut() + Send>>) {
        let sharing = self.sharing.clone();

        thread::spawn(move || {
            let sharing = sharing;
            let _active_guard = Registration::new(&sharing.active_tasks);

            if let Some(mut f) = initial_fn {
                f();
            }

            loop {
                let mut task: Box<dyn FnMut() + Send> = {
                    let mut todo = sharing.todo.lock().expect("任务队列锁中毒");

                    let task;
                    loop {
                        if let Some(poped_task) = todo.pop_front() {
                            task = poped_task;
                            break;
                        }
                        let _waiting_guard = Registration::new(&sharing.waiting_tasks);

                        let received = if sharing.active_tasks.load(Ordering::Acquire) <= MIN_THREADS {
                            todo = sharing.condvar.wait(todo).expect("任务队列锁中毒");
                            true
                        } else {
                            let (new_lock, waitres) = sharing
                                .condvar
                                .wait_timeout(todo, Duration::from_millis(5000))
                                .expect("任务队列锁中毒");
                            todo = new_lock;
                            !waitres.timed_out()
                        };

                        if !received && todo.is_empty() {
                            return;
                        }
                    }

                    task
                };

                task();
            }
        });
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.sharing.active_tasks.store(999_999_999, Ordering::Release);
        self.sharing.condvar.notify_all();
    }
}
