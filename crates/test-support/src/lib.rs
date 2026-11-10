//! 集成测试（`test_plan/`）共享支撑：真进程编排、资源生成、Prom/VM 查询与轮询。
//!
//! 目的是让每条场景**只写自己的断言**，不再各自复制一遍 `free_port` / 写 toml /
//! 起子进程 / 拼 envelope。三件事必须共用：
//!
//! 1. 进程与端口：`proc::Dataserver`（写配置 → 起真进程 → 等 `/health` → SIGTERM）。
//! 2. 资源生成：`data::*` 造 metrics / traces / eBPF 边 / eBPF 事件 / logs 的 envelope，
//!    以及带唯一前缀的批量点。**造出来的 record 必须能被产品代码接受**，这是共用它的
//!    前提——生成器写错会让所有用例一起假绿。
//! 3. 查询与轮询：`prom`（dataserver 的 `/api/v1/query`）与 `vm`（VictoriaMetrics 直查）。
//!    「落进 VM」只能由 `vm::Vm::query` 证明；dataserver 自己的查询口有缓存/内存态，
//!    不能作为落库证据。
//!
//! 用法：
//!
//! ```ignore
//! use vectorman_test_support::{data, proc::{Dataserver, Options}, prom, util, vm};
//!
//! let dir = tempfile::TempDir::new().unwrap();
//! let vm = vm::Vm::require();
//! let mut ds = Dataserver::start(dir.path(), Options::default().with_vm(&vm).with_apm(1));
//! ds.ingest(&data::metrics_envelope("b1", "a1", "h1", vec![data::metric_record(
//!     "r1", "tp_gauge", [("k", "v")], "value", 1.0, util::now_micros())]));
//! let got = prom::wait_query(ds.prom(), "tp_gauge", None, std::time::Duration::from_secs(20));
//! assert_eq!(got.value_sum(), 1.0);
//! ```

pub mod apm;
pub mod data;
pub mod http;
pub mod proc;
pub mod prom;
pub mod util;
pub mod vm;
