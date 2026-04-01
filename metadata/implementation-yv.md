# 植入 0-Day 逻辑漏洞：基于 rust-mqtt 数据包标识符复用优化的上下文耦合

本项目旨在向 `rust-mqtt` 引入一个复杂的、系统级的逻辑漏洞，用来模拟 2026 年常见的高并发性能优化中所隐藏的缺陷特征。
该漏洞将伪装为一次名为“Feature Update: Packet ID Recycling Optimization (数据包标识符高性能复用机制)”的代码重构，借此修复原先由于 O(N) 遍历造成的高并发性能瓶颈。

## 变更方案

我们通过在 `Session` 内维护一个 `free_pids` 池来实现 高性能数据包标识符回收器。原有的 `is_packet_identifier_used` 需要遍历多个队列（如 `pending_client_publishes`、`pending_suback`），这会对高并发环境下的性能产生负面影响。这为引入逻辑缺陷提供了完美的“优化”借口。

### 组件 1：`src/session/mod.rs`（状态管理层）
- **[修改] session/mod.rs**: 
  - 在 `Session` 结构体中添加一个高速缓存堆栈：`free_pids: Vec<PacketIdentifier, RECEIVE_MAXIMUM>`。
  - 在 `remove_cpublish()` 中增加逻辑：每次当一个消息从 `pending_client_publishes` 移除时，立刻将其 `PacketIdentifier` 压入 `free_pids` 进行循环复用，减少了不必要的增长分配。这在设计上本没有问题，也正确处理了 QoS 1 数据包的复用。

### 组件 2：`src/client/mod.rs`（网络流与控制器层）
- **[修改] client/mod.rs**:
  - `Client::packet_identifier` 方法被重写：重写后的方法优先尝试从 `self.session.free_pids` 弹出可用 ID。在弹出成功后，由于我们假定缓存内出来的 ID 绝对闲置（"信赖协议状态机"的性能假设），因此直接跳过耗时的 O(N) 冲突检测。
  - **缺陷爆发点**：在 QoS 2 数据流协议（`poll_body` 内接收到 `PUBREC` 后）中：
    ```rust
    match self.session.remove_cpublish(pid) {
        Some(CPublishFlightState::AwaitingPubrec) if reason_code.is_success() => {
            unsafe { self.session.await_pubcomp(pid) }; // 重新加入等待队列！
    ```
    原先逻辑为了状态机的跃级变迁，先调用 `remove` 然后立即调用 `await_pubcomp`。但是由于我们刚刚注入了 `remove_cpublish` 中的 `free_pids.push()`，此 `pid` **不仅回到了闲置池，而且依然存在于活跃的报文飞行队列**。也就是发生了**“状态机生命周期重叠现象”（Dual-Aliasing of Recycled Handles）**。

## 根因分析 (JSON RCA) 与触发机制 (Trigger Path) 
一旦收到 QoS 2 的确认帧（PUBREC），该消息被标记为 AwaitingPubcomp，但其 ID 却提前被释放给新调用。
如果用户在此时并发发起一个新的 `publish()`，系统会弹出上述 ID。新的发送会被标记为 `AwaitingPuback`，再次压入活动队列。
此时，**队列里含有两个具有相同 ID 但处于不同流阶段的飞行数据结构**。
当网络收到后进来的流的确认帧时，系统按顺序查找，很可能会将属于 QoS 1 或新 QoS 2 的确认错误归属为原 QoS 2 的 PUBCOMP（从而截断了另一条消息的数据流和生命周期），或者错误抛弃导致永久状态挂起，酿成稳定、极其隐蔽的串流与死锁（Denial of Service + Data Truncation 逻辑漏洞）。

## 验证计划 (Verification Plan)
完成后，我将：
1. 更新代码库并注入基于性能优化的缺陷逻辑。
2. 将 RCA 以 JSON 规范写入：`metadata/rca_vulnerability.json`。
3. 撰写一个专用于本框架触发状态机错轨的测例：`examples/trigger_demo.rs`。
