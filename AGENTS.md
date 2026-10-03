Architectural invariants:

- Preserve existing language and module ownership.
- A request to modify a module means the implementation should remain in that module unless explicitly stated otherwise.
- Do not replace substantive implementations with thin FFI/RPC/wrapper layers.
- Do not move functionality across Rust/C/C++/Go language boundaries merely to simplify implementation.
- Prefer the smallest local change that preserves existing architecture.
- Architectural changes require explicit justification and user approval.

Local project memory:

- Before project work, read `.local/notes/project-memory.md` if it exists. Reading and searching relevant files under `.local/` is allowed even though Git ignores them.
- Use memory as project context, verify historical state against the current repository, and follow current user instructions when they differ from stored notes.
- Keep `.local/` ignored by Git. Do not commit its contents or copy personal memory and execution records into tracked project documentation.
