### Pass's Execution Order and Priority Override

This document describes how to control Pass execution order and priority through configuration and environment variables, overriding compile-time hardcoded priorities.

#### Background and Objectives
- Default execution order is determined solely by `priority` in compile-time annotations (higher values execute first).
- Runtime priority configuration:
  - Explicit order: Define exact sequence in configuration file, run strictly by list, **unlisted passes will not run**.
  - Priority override: Provide new priority values for individual **Passes** by name without modifying source annotations.

> **Explicit order** > **Priority override** >= **Compile-time annotation priority**. When explicit order exists, priority becomes completely ineffective, running strictly by explicit order, and passes not in the explicit order will not run.
>
> To avoid unpredictable issues, each Pass will only run once!

#### Configuration Structure
```rust
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PassOrderConfig {
    /// Explicit execution order; if None, sort by priority
    pub order: Option<Vec<String>>,
    /// Override priority for each Pass (higher values come first)
    pub priority_override: HashMap<String, i32>,
}
```

Environment variable override implementation:
```rust
impl EnvOverlay for PassOrderConfig {
    fn overlay_env(&mut self) {
        // AMICE_PASS_ORDER="A,B,C" or "A;B;C"
        // AMICE_PASS_PRIORITY_OVERRIDE="A=1200,B=500" or "A=1200;B=500"
    }
}
```

#### Effective Priority and Rules
Execution order follows these rules:
1) If explicit order `pass_order.order` is provided in configuration:
- Passes in the list run strictly in the given order.
- Passes not appearing in the list **will not run**.
- If both explicit order and `priority_override` exist, `explicit order` takes precedence, `priority_override` is not executed.

2) If no explicit order is provided, but `pass_order.priority_override` is provided:
- Sort by overridden priority from high to low; non-overridden passes use default priority.

3) If neither is provided:
- Fall back to default behavior: sort by compile-time priority from high to low.

#### Configuration File Examples
TOML Example: TODO
YAML Example: TODO
JSON Example: TODO

#### Environment Variable Override
Supports temporary override without modifying configuration files:
- Explicit order
  - AMICE_PASS_ORDER="StringEncryption,SplitBasicBlock,ShuffleBlocks,IndirectBranch,IndirectCall"
  - Delimiters can be comma or semicolon: "A,B,C" or "A;B;C"
- Override priority
  - AMICE_PASS_PRIORITY_OVERRIDE="StringEncryption=1200,IndirectBranch=500"
  - Also supports semicolon as delimiter: "A=1200;B=500"

#### Pass Names and Default Priorities
Since names or priority values may change with updates, and wiki synchronization here may not be timely enough, here's a source code snippet (if you need to change execution order, please refer to the source code yourself):
```rust
#[amice(priority = 800, name = "IndirectBranch")]
#[derive(Default)]
pub struct IndirectBranch {
    enable: bool,
    flags: IndirectBranchFlags,
    xor_key: Option<[u32; 4]>,
}
```