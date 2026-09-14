# Zig schema-v1 基线

这些 JSON 和 patch 由迁移前的 Zig 实现（commit `45c1d47`，Zig 0.16.0）在隔离临时仓库中生成，不是 Rust 实现生成的预期结果。

- `working-tree.patch`：修改 `sample.rs` 后的完整上下文 Git diff。
- `comments.json`：用旧 CLI 创建的 new 侧多行评论与 old 侧评论。
- `review-states.json`：用旧 CLI 标记文件已审阅后的数据。
- `identity.json`：当时的仓库 realpath、repo_id 和工作区 target_id；路径只作为哈希输入，不需在本机存在。

`tests/migration.rs` 验证身份、patch/行锚点、schema 往返、追加评论及失效规则。新增 Rust 数据也已用旧二进制读回对照；日常测试不再依赖 Zig。
