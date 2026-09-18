# 公共推理调度层下沉

用户要求新增 `crates/common`，集中 ThreadManager、Queue 等通用定义，减少各模型模块的重复代码。

`docparse-common` 不依赖任何工作区业务 crate 或 ONNX。承接原生线程所有权、共享有界队列、就绪任务取批次、原生和浏览器平台适配、取消生命周期与计时上下文。模型模块只实现请求取消判定、模型初始化、输入合批、推理和结果转换。

原生 SessionManager 与 Texo 复用同一队列及线程关闭实现；原生 PP、MinerU 的独立异步执行器复用 ThreadManager；浏览器与原生 PP 复用异步接收和就绪批次收集。保持调用者批次原子入队、消费者跨批次取满、取消跳过、初始化失败清理、模型线程亲和性以及跨 Tokio runtime 复用。

根据后续要求删除 layout 的 timing 兼容层，全部计时引用直接依赖 common::timing；平台类型的兼容重导出保留。不改变配置字段、模型输出或浏览器全局推理互斥。迁移现有调度测试并增加公共异步队列和跨调用者批次回归；验证原生、WASM 和真实模型。

用户补充要求：检查各模块的 wasm_compat；core 的 TaskSet/spawn 已纳入公共层，PDF、字体数据库、模型后端与 HTTP 业务策略保留在所属模块。
