#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <stdbool.h>

// ===== Bytecode Header =====
#define VMP_MAGIC "VMP1"
#define VMP_VERSION 1u

// ===== Bytecode =====
typedef enum {
    OP_Push = 0,
    OP_Pop,
    OP_PopToReg,
    OP_PushFromReg,
    OP_ClearReg,

    OP_Alloca,
    OP_Alloca2,
    OP_Store,
    OP_StoreValue,
    OP_Load,
    OP_LoadValue,

    OP_Call,

    OP_Add,
    OP_Sub,
    OP_Mul,
    OP_Div,

    OP_Ret,

    OP_Nop,
    OP_Swap,
    OP_Dup,
    OP_TypeCheckInt,

    OP_Jump,
    OP_JumpIf,
    OP_JumpIfNot,

    OP_ICmpEq,
    OP_ICmpNe,
    OP_ICmpSlt,
    OP_ICmpSle,
    OP_ICmpSgt,
    OP_ICmpSge,
    OP_ICmpUlt,
    OP_ICmpUle,
    OP_ICmpUgt,
    OP_ICmpUge,

    OP_And,
    OP_Or,
    OP_Xor,
    OP_Shl,
    OP_LShr,
    OP_AShr,

    OP_Trunc,
    OP_ZExt,
    OP_SExt,
    OP_FPToSI,
    OP_FPToUI,
    OP_SIToFP,
    OP_UIToFP,

    OP_Label,
    OP_MetaGVar,
} OpCode;

// ===== Bytecode Value Type =====
typedef enum {
    VT_Undef = 0,
    VT_I1 = 1,
    VT_I8 = 2,
    VT_I16 = 3,
    VT_I32 = 4,
    VT_I64 = 5,
    VT_F32 = 6,
    VT_F64 = 7,
    VT_Ptr = 8,
} ValueTag;

typedef struct {
    ValueTag tag;

    union {
        uint8_t i1;
        int8_t i8;
        int16_t i16;
        int32_t i32;
        int64_t i64;
        float f32;
        double f64;
        uint64_t ptr;
    } v;
} VMPValue;

// ===== Vector =====
typedef struct {
    VMPValue *data;
    size_t len;
    size_t cap;
} ValueStack;

static void stack_init(ValueStack *s) {
    s->data = NULL;
    s->len = 0;
    s->cap = 0;
}

static void stack_free(ValueStack *s) {
    free(s->data);
    s->data = NULL;
    s->len = 0;
    s->cap = 0;
}

static void stack_reserve(ValueStack *s, size_t need) {
    if (need <= s->cap) return;
    size_t new_cap = s->cap ? s->cap : 16;
    while (new_cap < need) new_cap *= 2;
    VMPValue *p = (VMPValue *) realloc(s->data, new_cap * sizeof(VMPValue));
    if (!p) {
        fprintf(stderr, "[VM] OOM on stack realloc\n");
        exit(1);
    }
    s->data = p;
    s->cap = new_cap;
}

static void stack_push(ValueStack *s, VMPValue val) {
    stack_reserve(s, s->len + 1);
    s->data[s->len++] = val;
}

static int stack_pop(ValueStack *s, VMPValue *out) {
    if (s->len == 0) return -1;
    *out = s->data[--s->len];
    return 0;
}

static int stack_peek(ValueStack *s, VMPValue *out) {
    if (s->len == 0) return -1;
    *out = s->data[s->len - 1];
    return 0;
}

// ===== RegList =====
typedef struct {
    uint32_t reg;
    VMPValue val;
    int in_use;
} RegEntry;

typedef struct {
    RegEntry *data;
    size_t len;
    size_t cap;
} RegTable;

static void regs_init(RegTable *t) {
    t->data = NULL;
    t->len = 0;
    t->cap = 0;
}

static void regs_free(RegTable *t) {
    free(t->data);
    t->data = NULL;
    t->len = 0;
    t->cap = 0;
}

static void regs_reserve(RegTable *t, size_t need) {
    if (need <= t->cap) return;
    size_t new_cap = t->cap ? t->cap : 16;
    while (new_cap < need) new_cap *= 2;
    RegEntry *p = (RegEntry *) realloc(t->data, new_cap * sizeof(RegEntry));
    if (!p) {
        fprintf(stderr, "[VM] OOM on regs realloc\n");
        exit(1);
    }
    t->data = p;
    for (size_t i = t->cap; i < new_cap; ++i) t->data[i].in_use = 0;
    t->cap = new_cap;
}

static int regs_get(RegTable *t, uint32_t reg, VMPValue *out) {
    for (size_t i = 0; i < t->len; ++i) {
        if (t->data[i].in_use && t->data[i].reg == reg) {
            *out = t->data[i].val;
            return 0;
        }
    }
    return -1;
}

static void regs_set(RegTable *t, uint32_t reg, VMPValue v) {
    for (size_t i = 0; i < t->len; ++i) {
        if (t->data[i].in_use && t->data[i].reg == reg) {
            t->data[i].val = v;
            return;
        }
    }
    regs_reserve(t, t->len + 1);
    t->data[t->len].reg = reg;
    t->data[t->len].val = v;
    t->data[t->len].in_use = 1;
    t->len += 1;
}

static void regs_clear(RegTable *t, uint32_t reg) {
    for (size_t i = 0; i < t->len; ++i) {
        if (t->data[i].in_use && t->data[i].reg == reg) {
            t->data[i].in_use = 0;
            return;
        }
    }
}

// ===== Label Table（hash -> 指令索引），线性表实现 =====
typedef struct {
    uint64_t hash;
    size_t pc_index;
} LabelEntry;

typedef struct {
    LabelEntry *data;
    size_t len;
    size_t cap;
} LabelTable;

static void labels_init(LabelTable *t) {
    t->data = NULL;
    t->len = 0;
    t->cap = 0;
}

static void labels_free(LabelTable *t) {
    free(t->data);
    t->data = NULL;
    t->len = 0;
    t->cap = 0;
}

static void labels_reserve(LabelTable *t, size_t need) {
    if (need <= t->cap) return;
    size_t new_cap = t->cap ? t->cap : 16;
    while (new_cap < need) new_cap *= 2;
    LabelEntry *p = (LabelEntry *) realloc(t->data, new_cap * sizeof(LabelEntry));
    if (!p) {
        fprintf(stderr, "[VM] OOM on labels realloc\n");
        exit(1);
    }
    t->data = p;
    t->cap = new_cap;
}

static void labels_add(LabelTable *t, uint64_t hash, size_t pc_index) {
    labels_reserve(t, t->len + 1);
    t->data[t->len].hash = hash;
    t->data[t->len].pc_index = pc_index;
    t->len += 1;
}

static int labels_find(const LabelTable *t, uint64_t hash, size_t *out_index) {
    for (size_t i = 0; i < t->len; ++i) {
        if (t->data[i].hash == hash) {
            *out_index = t->data[i].pc_index;
            return 0;
        }
    }
    return -1;
}

// ===== VM 内存（堆） =====
typedef struct {
    uint8_t *data;
    size_t size;
    size_t next_addr; // 线性分配，从 0x1000 开始
    int debug;
} VMMemory;

static void mem_init(VMMemory *m) {
    m->size = 1024 * 1024; // 1MB
    m->data = (uint8_t *) calloc(1, m->size);
    if (!m->data) {
        fprintf(stderr, "[VM] OOM on memory init\n");
        exit(1);
    }
    m->next_addr = 0x1000;
    m->debug = 0;
}

static void mem_free(VMMemory *m) {
    free(m->data);
    m->data = NULL;
    m->size = 0;
    m->next_addr = 0;
}

static void mem_ensure(VMMemory *m, size_t need) {
    if (need <= m->size) return;
    size_t new_size = m->size;
    while (new_size < need) new_size = new_size + new_size / 2 + 4096;
    uint8_t *p = (uint8_t *) realloc(m->data, new_size);
    if (!p) {
        fprintf(stderr, "[VM] OOM on memory grow\n");
        exit(1);
    }
    // 新增区域清零
    memset(p + m->size, 0, new_size - m->size);
    m->data = p;
    m->size = new_size;
}

static size_t mem_alloc(VMMemory *m, size_t payload_size) {
    // 对象布局: [1字节 类型标签] + [payload_size 字节数据]
    size_t addr = m->next_addr;
    size_t need = addr + 1 + payload_size;
    mem_ensure(m, need + 1024);
    m->next_addr = need; // 线性 bump 分配
    return addr + 1; // 返回数据区起始地址
}

static void mem_store_value(VMMemory *m, size_t addr, const VMPValue *val) {
    uint8_t tag = (uint8_t) val->tag;
    size_t sz = 0;
    switch (val->tag) {
        case VT_Undef: sz = 0;
            break;
        case VT_I1: sz = 1;
            break;
        case VT_I8: sz = 1;
            break;
        case VT_I16: sz = 2;
            break;
        case VT_I32: sz = 4;
            break;
        case VT_I64: sz = 8;
            break;
        case VT_F32: sz = 4;
            break;
        case VT_F64: sz = 8;
            break;
        case VT_Ptr: sz = 8;
            break;
    }
    mem_ensure(m, addr + 1 + sz);
    m->data[addr - 1] = tag;
    if (sz) {
        switch (val->tag) {
            case VT_Undef: break;
            case VT_I1: m->data[addr] = val->v.i1 ? 1 : 0;
                break;
            case VT_I8: memcpy(m->data + addr, &val->v.i8, 1);
                break;
            case VT_I16: memcpy(m->data + addr, &val->v.i16, 2);
                break;
            case VT_I32: memcpy(m->data + addr, &val->v.i32, 4);
                break;
            case VT_I64: memcpy(m->data + addr, &val->v.i64, 8);
                break;
            case VT_F32: memcpy(m->data + addr, &val->v.f32, 4);
                break;
            case VT_F64: memcpy(m->data + addr, &val->v.f64, 8);
                break;
            case VT_Ptr: memcpy(m->data + addr, &val->v.ptr, 8);
                break;
        }
    }
}

static int mem_load_value(VMMemory *m, size_t addr, VMPValue *out) {
    if (addr >= m->size) return -1;
    uint8_t tag = m->data[addr - 1];
    out->tag = (ValueTag) tag;
    switch (out->tag) {
        case VT_Undef:
            return 0;
        case VT_I1:
            out->v.i1 = m->data[addr] ? 1 : 0;
            return 0;
        case VT_I8:
            memcpy(&out->v.i8, m->data + addr, 1);
            return 0;
        case VT_I16:
            memcpy(&out->v.i16, m->data + addr, 2);
            return 0;
        case VT_I32:
            memcpy(&out->v.i32, m->data + addr, 4);
            return 0;
        case VT_I64:
            memcpy(&out->v.i64, m->data + addr, 8);
            return 0;
        case VT_F32:
            memcpy(&out->v.f32, m->data + addr, 4);
            return 0;
        case VT_F64:
            memcpy(&out->v.f64, m->data + addr, 8);
            return 0;
        case VT_Ptr:
            memcpy(&out->v.ptr, m->data + addr, 8);
            return 0;
    }
    return -1;
}

// ===== 指令表示 =====
typedef struct {
    OpCode op;

    union {
        struct {
            VMPValue value;
        } Push;

        struct {
            uint32_t reg;
        } RegU32;

        struct {
            uint64_t addr;
        } MemAddr;

        struct {
            uint64_t func_hash;
            uint8_t is_void;
            uint32_t arg_num;
        } Call;

        struct {
            uint8_t flags;
        } Add;

        struct {
            uint32_t width;
        } TypeCk;

        struct {
            uint64_t target_hash;
        } Jump;

        struct {
            uint64_t label_hash;
        } Label;

        struct {
            uint64_t size;
        } Alloca;
    } u;
} Instruction;

typedef struct {
    // 状态
    ValueStack stack;
    RegTable regs;
    VMMemory mem;
    LabelTable labels;
    int debug;

    // 统计
    uint64_t instructions_executed;
    uint64_t function_calls;
    uint64_t memory_allocations;
    size_t stack_max_depth;
} VM;

static void vm_init(VM *vm) {
    stack_init(&vm->stack);
    regs_init(&vm->regs);
    mem_init(&vm->mem);
    labels_init(&vm->labels);
    vm->debug = 0;
    vm->instructions_executed = 0;
    vm->function_calls = 0;
    vm->memory_allocations = 0;
    vm->stack_max_depth = 0;
}

static void vm_free(VM *vm) {
    stack_free(&vm->stack);
    regs_free(&vm->regs);
    mem_free(&vm->mem);
    labels_free(&vm->labels);
}

// ===== 小端读取工具 =====
static int read_u16(const uint8_t *p, size_t n, size_t *off, uint16_t *out) {
    if (*off + 2 > n) return -1;
    *out = (uint16_t) (p[*off] | ((uint16_t) p[*off + 1] << 8));
    *off += 2;
    return 0;
}

static int read_u32(const uint8_t *p, size_t n, size_t *off, uint32_t *out) {
    if (*off + 4 > n) return -1;
    *out = (uint32_t) (
        ((uint32_t) p[*off + 0]) |
        ((uint32_t) p[*off + 1] << 8) |
        ((uint32_t) p[*off + 2] << 16) |
        ((uint32_t) p[*off + 3] << 24)
    );
    *off += 4;
    return 0;
}

static int read_u64(const uint8_t *p, size_t n, size_t *off, uint64_t *out) {
    if (*off + 8 > n) return -1;
    uint64_t v = 0;
    for (int i = 0; i < 8; ++i) v |= ((uint64_t) p[*off + i]) << (8 * i);
    *out = v;
    *off += 8;
    return 0;
}

static int read_f32(const uint8_t *p, size_t n, size_t *off, float *out) {
    if (*off + 4 > n) return -1;
    memcpy(out, p + *off, 4);
    *off += 4;
    return 0;
}

static int read_f64(const uint8_t *p, size_t n, size_t *off, double *out) {
    if (*off + 8 > n) return -1;
    memcpy(out, p + *off, 8);
    *off += 8;
    return 0;
}

static int read_usize_as_u64(const uint8_t *p, size_t n, size_t *off, uint64_t *out) {
    // 编码端使用 usize::to_le_bytes()，在 64-bit 平台为 8 字节
    return read_u64(p, n, off, out);
}

// 读取 VMPValue（先读 1 字节类型码，再读具体字节）
static int read_value(const uint8_t *p, size_t n, size_t *off, VMPValue *out) {
    if (*off + 1 > n) return -1;
    uint8_t t = p[*off];
    *off += 1;
    out->tag = (ValueTag) t;
    switch (out->tag) {
        case VT_Undef: return 0;
        case VT_I1: if (*off + 1 > n) return -1;
            out->v.i1 = p[*off] ? 1 : 0;
            *off += 1;
            return 0;
        case VT_I8: if (*off + 1 > n) return -1;
            memcpy(&out->v.i8, p + *off, 1);
            *off += 1;
            return 0;
        case VT_I16: if (*off + 2 > n) return -1;
            memcpy(&out->v.i16, p + *off, 2);
            *off += 2;
            return 0;
        case VT_I32: if (*off + 4 > n) return -1;
            memcpy(&out->v.i32, p + *off, 4);
            *off += 4;
            return 0;
        case VT_I64: if (*off + 8 > n) return -1;
            memcpy(&out->v.i64, p + *off, 8);
            *off += 8;
            return 0;
        case VT_F32: return read_f32(p, n, off, &out->v.f32);
        case VT_F64: return read_f64(p, n, off, &out->v.f64);
        case VT_Ptr: return read_u64(p, n, off, &out->v.ptr);
    }
    return -1;
}

// ===== 指令解码 =====
typedef struct {
    Instruction *data;
    size_t len;
    size_t cap;
} InstList;

static void insts_init(InstList *l) {
    l->data = NULL;
    l->len = 0;
    l->cap = 0;
}

static void insts_free(InstList *l) {
    free(l->data);
    l->data = NULL;
    l->len = 0;
    l->cap = 0;
}

static void insts_reserve(InstList *l, size_t need) {
    if (need <= l->cap) return;
    size_t new_cap = l->cap ? l->cap : 64;
    while (new_cap < need) new_cap *= 2;
    Instruction *p = (Instruction *) realloc(l->data, new_cap * sizeof(Instruction));
    if (!p) {
        fprintf(stderr, "[VM] OOM on insts realloc\n");
        exit(1);
    }
    l->data = p;
    l->cap = new_cap;
}

static int decode_bytecode(const uint8_t *buf, size_t len, InstList *out_insts, LabelTable *out_labels, int debug) {
    size_t off = 0;
    if (len < 8) {
        fprintf(stderr, "[VM] bytecode too short\n");
        return -1;
    }
    if (memcmp(buf, VMP_MAGIC, 4) != 0) {
        fprintf(stderr, "[VM] bad magic\n");
        return -1;
    }
    off += 4;
    uint32_t ver = 0;
    if (read_u32(buf, len, &off, &ver)) {
        fprintf(stderr, "[VM] bad header ver\n");
        return -1;
    }
    if (ver != VMP_VERSION) {
        fprintf(stderr, "[VM] unsupported version %u\n", ver);
        return -1;
    }

    insts_init(out_insts);
    labels_init(out_labels);

    while (off < len) {
        uint16_t op16 = 0;
        if (read_u16(buf, len, &off, &op16)) {
            fprintf(stderr, "[VM] truncated opcode\n");
            return -1;
        }
        Instruction inst;
        memset(&inst, 0, sizeof(inst));
        inst.op = (OpCode) op16;
        switch (inst.op) {
            case OP_Push: {
                if (read_value(buf, len, &off, &inst.u.Push.value)) return -1;
                break;
            }
            case OP_Pop: {
                break;
            }
            case OP_PopToReg: {
                if (read_u32(buf, len, &off, &inst.u.RegU32.reg)) return -1;
                break;
            }
            case OP_PushFromReg: {
                if (read_u32(buf, len, &off, &inst.u.RegU32.reg)) return -1;
                break;
            }
            case OP_ClearReg: {
                if (read_u32(buf, len, &off, &inst.u.RegU32.reg)) return -1;
                break;
            }
            case OP_Alloca: {
                if (read_usize_as_u64(buf, len, &off, &inst.u.Alloca.size)) return -1;
                break;
            }
            case OP_Alloca2: {
                break;
            }
            case OP_Store: {
                if (read_usize_as_u64(buf, len, &off, &inst.u.MemAddr.addr)) return -1;
                break;
            }
            case OP_StoreValue: {
                break;
            }
            case OP_Load: {
                if (read_usize_as_u64(buf, len, &off, &inst.u.MemAddr.addr)) return -1;
                break;
            }
            case OP_LoadValue: {
                break;
            }
            case OP_Call: {
                if (read_u64(buf, len, &off, &inst.u.Call.func_hash)) return -1;
                if (off + 1 > len) return -1;
                inst.u.Call.is_void = buf[off++];
                if (read_u32(buf, len, &off, &inst.u.Call.arg_num)) return -1;
                break;
            }
            case OP_Add: {
                // flags + padding(1 byte)
                if (off + 2 > len) return -1;
                inst.u.Add.flags = buf[off];
                off += 2;
                break;
            }
            case OP_Sub:
            case OP_Mul:
            case OP_Div:
            case OP_Ret:
            case OP_Nop:
            case OP_Swap:
            case OP_Dup: {
                break;
            }
            case OP_TypeCheckInt: {
                if (read_u32(buf, len, &off, &inst.u.TypeCk.width)) return -1;
                break;
            }
            case OP_Jump:
            case OP_JumpIf:
            case OP_JumpIfNot: {
                if (read_u64(buf, len, &off, &inst.u.Jump.target_hash)) return -1;
                break;
            }
            case OP_ICmpEq:
            case OP_ICmpNe:
            case OP_ICmpSlt:
            case OP_ICmpSle:
            case OP_ICmpSgt:
            case OP_ICmpSge:
            case OP_ICmpUlt:
            case OP_ICmpUle:
            case OP_ICmpUgt:
            case OP_ICmpUge: {
                // 未实现，运行期遇到时报错
                break;
            }
            case OP_And:
            case OP_Or:
            case OP_Xor:
            case OP_Shl:
            case OP_LShr:
            case OP_AShr: {
                // 未实现，运行期遇到时报错
                break;
            }
            case OP_Trunc:
            case OP_ZExt:
            case OP_SExt:
            case OP_FPToSI:
            case OP_FPToUI:
            case OP_SIToFP:
            case OP_UIToFP: {
                // 对应各自参数（已在 encoder 写入），但现阶段执行未实现
                // 读取并丢弃以保持游标前进
                if (inst.op == OP_SIToFP || inst.op == OP_UIToFP) {
                    if (off + 1 > len) return -1;
                    off += 1; // is_double u8
                } else {
                    // u32 target_width
                    uint32_t dummy = 0;
                    if (read_u32(buf, len, &off, &dummy)) return -1;
                }
                break;
            }
            case OP_Label: {
                if (read_u64(buf, len, &off, &inst.u.Label.label_hash)) return -1;
                break;
            }
            case OP_MetaGVar: {
                // 编码端未写入任何附加字段
                break;
            }
            default: {
                fprintf(stderr, "[VM] unknown opcode %u\n", (unsigned) op16);
                return -1;
            }
        }

        // 记录指令
        insts_reserve(out_insts, out_insts->len + 1);
        out_insts->data[out_insts->len] = inst;

        // 若为 Label，建立 hash -> 指令索引 的映射
        if (inst.op == OP_Label) {
            labels_add(out_labels, inst.u.Label.label_hash, out_insts->len);
        }

        out_insts->len += 1;
    }

    if (debug) {
        fprintf(stderr, "[VM] decoded %zu instructions, %zu labels\n", out_insts->len, out_labels->len);
    }
    return 0;
}

// ===== VMPValue 工具 =====
static size_t value_size_in_bytes(const VMPValue *v) {
    switch (v->tag) {
        case VT_Undef: return 0;
        case VT_I1: return 1;
        case VT_I8: return 1;
        case VT_I16: return 2;
        case VT_I32: return 4;
        case VT_I64: return 8;
        case VT_F32: return 4;
        case VT_F64: return 8;
        case VT_Ptr: return 8;
    }
    return 0;
}

static size_t value_width_bits(const VMPValue *v) {
    if (v->tag == VT_Undef) return 0;
    return value_size_in_bytes(v) * 8;
}

static int value_is_true(const VMPValue *v) {
    switch (v->tag) {
        case VT_Undef: return 0;
        case VT_I1: return v->v.i1 != 0;
        case VT_I8: return v->v.i8 != 0;
        case VT_I16: return v->v.i16 != 0;
        case VT_I32: return v->v.i32 != 0;
        case VT_I64: return v->v.i64 != 0;
        case VT_F32: return v->v.f32 != 0.0f;
        case VT_F64: return v->v.f64 != 0.0;
        case VT_Ptr: return v->v.ptr != 0;
    }
    return 0;
}

static int add_values(const VMPValue *lhs, const VMPValue *rhs, VMPValue *out) {
    if (lhs->tag == VT_I32 && rhs->tag == VT_I32) {
        out->tag = VT_I32;
        out->v.i32 = lhs->v.i32 + rhs->v.i32;
        return 0;
    }
    if (lhs->tag == VT_I64 && rhs->tag == VT_I64) {
        out->tag = VT_I64;
        out->v.i64 = lhs->v.i64 + rhs->v.i64;
        return 0;
    }
    if (lhs->tag == VT_F32 && rhs->tag == VT_F32) {
        out->tag = VT_F32;
        out->v.f32 = lhs->v.f32 + rhs->v.f32;
        return 0;
    }
    if (lhs->tag == VT_F64 && rhs->tag == VT_F64) {
        out->tag = VT_F64;
        out->v.f64 = lhs->v.f64 + rhs->v.f64;
        return 0;
    }
    if (lhs->tag == VT_Ptr && rhs->tag == VT_I64) {
        out->tag = VT_Ptr;
        out->v.ptr = lhs->v.ptr + (uint64_t) rhs->v.i64;
        return 0;
    }
    return -1;
}

static int sub_values(const VMPValue *lhs, const VMPValue *rhs, VMPValue *out) {
    if (lhs->tag == VT_I32 && rhs->tag == VT_I32) {
        out->tag = VT_I32;
        out->v.i32 = lhs->v.i32 - rhs->v.i32;
        return 0;
    }
    if (lhs->tag == VT_I64 && rhs->tag == VT_I64) {
        out->tag = VT_I64;
        out->v.i64 = lhs->v.i64 - rhs->v.i64;
        return 0;
    }
    if (lhs->tag == VT_F32 && rhs->tag == VT_F32) {
        out->tag = VT_F32;
        out->v.f32 = lhs->v.f32 - rhs->v.f32;
        return 0;
    }
    if (lhs->tag == VT_F64 && rhs->tag == VT_F64) {
        out->tag = VT_F64;
        out->v.f64 = lhs->v.f64 - rhs->v.f64;
        return 0;
    }
    return -1;
}

static int mul_values(const VMPValue *lhs, const VMPValue *rhs, VMPValue *out) {
    if (lhs->tag == VT_I32 && rhs->tag == VT_I32) {
        out->tag = VT_I32;
        out->v.i32 = lhs->v.i32 * rhs->v.i32;
        return 0;
    }
    if (lhs->tag == VT_I64 && rhs->tag == VT_I64) {
        out->tag = VT_I64;
        out->v.i64 = lhs->v.i64 * rhs->v.i64;
        return 0;
    }
    if (lhs->tag == VT_F32 && rhs->tag == VT_F32) {
        out->tag = VT_F32;
        out->v.f32 = lhs->v.f32 * rhs->v.f32;
        return 0;
    }
    if (lhs->tag == VT_F64 && rhs->tag == VT_F64) {
        out->tag = VT_F64;
        out->v.f64 = lhs->v.f64 * rhs->v.f64;
        return 0;
    }
    return -1;
}

static int div_values(const VMPValue *lhs, const VMPValue *rhs, VMPValue *out) {
    if (lhs->tag == VT_I32 && rhs->tag == VT_I32) {
        if (rhs->v.i32 == 0) return -1;
        out->tag = VT_I32;
        out->v.i32 = lhs->v.i32 / rhs->v.i32;
        return 0;
    }
    if (lhs->tag == VT_I64 && rhs->tag == VT_I64) {
        if (rhs->v.i64 == 0) return -1;
        out->tag = VT_I64;
        out->v.i64 = lhs->v.i64 / rhs->v.i64;
        return 0;
    }
    if (lhs->tag == VT_F32 && rhs->tag == VT_F32) {
        out->tag = VT_F32;
        out->v.f32 = lhs->v.f32 / rhs->v.f32;
        return 0;
    }
    if (lhs->tag == VT_F64 && rhs->tag == VT_F64) {
        out->tag = VT_F64;
        out->v.f64 = lhs->v.f64 / rhs->v.f64;
        return 0;
    }
    return -1;
}

// ===== VM 执行 =====
static int vm_execute(VM *vm, const Instruction *insts, size_t inst_count, VMPValue *ret_out) {
    size_t pc = 0;
    while (pc < inst_count) {
        const Instruction *I = &insts[pc];
        switch (I->op) {
            case OP_Push: {
                stack_push(&vm->stack, I->u.Push.value);
                break;
            }
            case OP_Pop: {
                VMPValue tmp;
                if (stack_pop(&vm->stack, &tmp)) {
                    fprintf(stderr, "[VM] stack underflow on Pop at pc %zu\n", pc);
                    return -1;
                }
                break;
            }
            case OP_PopToReg: {
                VMPValue v;
                if (stack_pop(&vm->stack, &v)) {
                    fprintf(stderr, "[VM] stack underflow on PopToReg at pc %zu\n", pc);
                    return -1;
                }
                regs_set(&vm->regs, I->u.RegU32.reg, v);
                break;
            }
            case OP_PushFromReg: {
                VMPValue v;
                if (regs_get(&vm->regs, I->u.RegU32.reg, &v)) {
                    fprintf(stderr, "[VM] reg %u not found at pc %zu\n", I->u.RegU32.reg, pc);
                    return -1;
                }
                stack_push(&vm->stack, v);
                break;
            }
            case OP_ClearReg: {
                regs_clear(&vm->regs, I->u.RegU32.reg);
                break;
            }
            case OP_Alloca: {
                size_t addr = mem_alloc(&vm->mem, (size_t) I->u.Alloca.size);
                VMPValue v;
                v.tag = VT_Ptr;
                v.v.ptr = (uint64_t) addr;
                stack_push(&vm->stack, v);
                vm->memory_allocations += 1;
                break;
            }
            case OP_Alloca2: {
                VMPValue szv;
                if (stack_pop(&vm->stack, &szv)) {
                    fprintf(stderr, "[VM] stack underflow on Alloca2 at pc %zu\n", pc);
                    return -1;
                }
                size_t sz = 0;
                if (szv.tag == VT_I64) sz = (size_t) szv.v.i64;
                else if (szv.tag == VT_I32) sz = (size_t) szv.v.i32;
                else {
                    fprintf(stderr, "[VM] invalid size type on Alloca2 at pc %zu\n", pc);
                    return -1;
                }
                size_t addr = mem_alloc(&vm->mem, sz);
                VMPValue v;
                v.tag = VT_Ptr;
                v.v.ptr = (uint64_t) addr;
                stack_push(&vm->stack, v);
                vm->memory_allocations += 1;
                break;
            }
            case OP_Store: {
                VMPValue v;
                if (stack_pop(&vm->stack, &v)) {
                    fprintf(stderr, "[VM] stack underflow on Store at pc %zu\n", pc);
                    return -1;
                }
                mem_store_value(&vm->mem, (size_t) I->u.MemAddr.addr, &v);
                break;
            }
            case OP_StoreValue: {
                VMPValue val, ptr;
                if (stack_pop(&vm->stack, &val) || stack_pop(&vm->stack, &ptr)) {
                    fprintf(stderr, "[VM] stack underflow on StoreValue at pc %zu\n", pc);
                    return -1;
                }
                if (ptr.tag != VT_Ptr) {
                    fprintf(stderr, "[VM] invalid pointer on StoreValue at pc %zu\n", pc);
                    return -1;
                }
                mem_store_value(&vm->mem, (size_t) ptr.v.ptr, &val);
                break;
            }
            case OP_Load: {
                VMPValue v;
                if (mem_load_value(&vm->mem, (size_t) I->u.MemAddr.addr, &v)) {
                    fprintf(stderr, "[VM] load OOB at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, v);
                break;
            }
            case OP_LoadValue: {
                VMPValue ptr;
                if (stack_pop(&vm->stack, &ptr)) {
                    fprintf(stderr, "[VM] stack underflow on LoadValue at pc %zu\n", pc);
                    return -1;
                }
                if (ptr.tag != VT_Ptr) {
                    fprintf(stderr, "[VM] invalid pointer on LoadValue at pc %zu\n", pc);
                    return -1;
                }
                VMPValue v;
                if (mem_load_value(&vm->mem, (size_t) ptr.v.ptr, &v)) {
                    fprintf(stderr, "[VM] load OOB at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, v);
                break;
            }
            case OP_Call: {
                // 简化：仅弹出参数，不实际调用；如非void，可推入 0
                for (uint32_t i = 0; i < I->u.Call.arg_num; ++i) {
                    VMPValue tmp;
                    if (stack_pop(&vm->stack, &tmp)) {
                        fprintf(stderr, "[VM] stack underflow on Call args at pc %zu\n", pc);
                        return -1;
                    }
                }
                if (!I->u.Call.is_void) {
                    VMPValue zero;
                    zero.tag = VT_I32;
                    zero.v.i32 = 0;
                    stack_push(&vm->stack, zero);
                }
                vm->function_calls += 1;
                break;
            }
            case OP_Add: {
                VMPValue rhs, lhs, res;
                if (stack_pop(&vm->stack, &rhs) || stack_pop(&vm->stack, &lhs)) {
                    fprintf(stderr, "[VM] stack underflow on Add at pc %zu\n", pc);
                    return -1;
                }
                if (add_values(&lhs, &rhs, &res)) {
                    fprintf(stderr, "[VM] type mismatch on Add at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, res);
                break;
            }
            case OP_Sub: {
                VMPValue rhs, lhs, res;
                if (stack_pop(&vm->stack, &rhs) || stack_pop(&vm->stack, &lhs)) {
                    fprintf(stderr, "[VM] stack underflow on Sub at pc %zu\n", pc);
                    return -1;
                }
                if (sub_values(&lhs, &rhs, &res)) {
                    fprintf(stderr, "[VM] type mismatch on Sub at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, res);
                break;
            }
            case OP_Mul: {
                VMPValue rhs, lhs, res;
                if (stack_pop(&vm->stack, &rhs) || stack_pop(&vm->stack, &lhs)) {
                    fprintf(stderr, "[VM] stack underflow on Mul at pc %zu\n", pc);
                    return -1;
                }
                if (mul_values(&lhs, &rhs, &res)) {
                    fprintf(stderr, "[VM] type mismatch on Mul at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, res);
                break;
            }
            case OP_Div: {
                VMPValue rhs, lhs, res;
                if (stack_pop(&vm->stack, &rhs) || stack_pop(&vm->stack, &lhs)) {
                    fprintf(stderr, "[VM] stack underflow on Div at pc %zu\n", pc);
                    return -1;
                }
                if (div_values(&lhs, &rhs, &res)) {
                    fprintf(stderr, "[VM] div error or type mismatch at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, res);
                break;
            }
            case OP_Ret: {
                if (ret_out) {
                    if (vm->stack.len) *ret_out = vm->stack.data[vm->stack.len - 1];
                    else { ret_out->tag = VT_Undef; }
                }
                return 0;
            }
            case OP_Nop: {
                break;
            }
            case OP_Swap: {
                if (vm->stack.len < 2) {
                    fprintf(stderr, "[VM] stack underflow on Swap at pc %zu\n", pc);
                    return -1;
                }
                VMPValue tmp = vm->stack.data[vm->stack.len - 1];
                vm->stack.data[vm->stack.len - 1] = vm->stack.data[vm->stack.len - 2];
                vm->stack.data[vm->stack.len - 2] = tmp;
                break;
            }
            case OP_Dup: {
                VMPValue top;
                if (stack_peek(&vm->stack, &top)) {
                    fprintf(stderr, "[VM] stack underflow on Dup at pc %zu\n", pc);
                    return -1;
                }
                stack_push(&vm->stack, top);
                break;
            }
            case OP_TypeCheckInt: {
                VMPValue top;
                if (stack_peek(&vm->stack, &top)) {
                    fprintf(stderr, "[VM] stack underflow on TypeCheckInt at pc %zu\n", pc);
                    return -1;
                }
                size_t width = value_width_bits(&top);
                if (width != (size_t) I->u.TypeCk.width) {
                    fprintf(stderr, "[VM] Type check failed at pc %zu: expect %u-bit, got %zu-bit\n", pc,
                            I->u.TypeCk.width, width);
                    return -1;
                }
                break;
            }
            case OP_Jump: {
                size_t target = 0;
                if (labels_find(&vm->labels, I->u.Jump.target_hash, &target)) {
                    fprintf(stderr, "[VM] label not found on Jump at pc %zu\n", pc);
                    return -1;
                }
                pc = target;
                goto after_pc_update;
            }
            case OP_JumpIf: {
                VMPValue cond;
                if (stack_pop(&vm->stack, &cond)) {
                    fprintf(stderr, "[VM] stack underflow on JumpIf at pc %zu\n", pc);
                    return -1;
                }
                if (value_is_true(&cond)) {
                    size_t target = 0;
                    if (labels_find(&vm->labels, I->u.Jump.target_hash, &target)) {
                        fprintf(stderr, "[VM] label not found on JumpIf at pc %zu\n", pc);
                        return -1;
                    }
                    pc = target;
                    goto after_pc_update;
                }
                break;
            }
            case OP_JumpIfNot: {
                VMPValue cond;
                if (stack_pop(&vm->stack, &cond)) {
                    fprintf(stderr, "[VM] stack underflow on JumpIfNot at pc %zu\n", pc);
                    return -1;
                }
                if (!value_is_true(&cond)) {
                    size_t target = 0;
                    if (labels_find(&vm->labels, I->u.Jump.target_hash, &target)) {
                        fprintf(stderr, "[VM] label not found on JumpIfNot at pc %zu\n", pc);
                        return -1;
                    }
                    pc = target;
                    goto after_pc_update;
                }
                break;
            }
            case OP_ICmpEq:
            case OP_ICmpNe:
            case OP_ICmpSlt:
            case OP_ICmpSle:
            case OP_ICmpSgt:
            case OP_ICmpSge:
            case OP_ICmpUlt:
            case OP_ICmpUle:
            case OP_ICmpUgt:
            case OP_ICmpUge:
            case OP_And:
            case OP_Or:
            case OP_Xor:
            case OP_Shl:
            case OP_LShr:
            case OP_AShr:
            case OP_Trunc:
            case OP_ZExt:
            case OP_SExt:
            case OP_FPToSI:
            case OP_FPToUI:
            case OP_SIToFP:
            case OP_UIToFP: {
                fprintf(stderr, "[VM] opcode %u not implemented at pc %zu\n", (unsigned) I->op, pc);
                return -1;
            }
            case OP_Label: {
                // 无操作
                break;
            }
            case OP_MetaGVar: {
                // 二进制未包含 reg/name，视为 NOP
                break;
            }
            default: {
                fprintf(stderr, "[VM] invalid opcode %u at pc %zu\n", (unsigned) I->op, pc);
                return -1;
            }
        }

        vm->instructions_executed += 1;
        if (vm->stack.len > vm->stack_max_depth) vm->stack_max_depth = vm->stack.len;
        pc += 1;
    after_pc_update:
        ;
    }
    // 程序自然结束，返回栈顶或 Undef
    if (ret_out) {
        if (vm->stack.len) *ret_out = vm->stack.data[vm->stack.len - 1];
        else ret_out->tag = VT_Undef;
    }
    return 0;
}

// ===== 外部 API =====
// 返回 0 表示成功，其余为错误；如需结果，可传 ret_out
int vmp_run_bytecode(const uint8_t *bytecode, size_t bytecode_len, int debug, VMPValue *ret_out) {
    VM vm;
    vm_init(&vm);
    vm.debug = debug;
    vm.mem.debug = debug;
    InstList insts;
    LabelTable labels;
    if (decode_bytecode(bytecode, bytecode_len, &insts, &labels, debug)) {
        vm_free(&vm);
        return -1;
    }
    // 将解析阶段收集的 labels 安装到 VM
    labels_free(&vm.labels);
    vm.labels = labels; // 直接接管所有权
    int rc = vm_execute(&vm, insts.data, insts.len, ret_out);
    if (debug) {
        fprintf(stderr, "=== Execution Statistics ===\n");
        fprintf(stderr, "Instructions executed: %llu\n", (unsigned long long) vm.instructions_executed);
        fprintf(stderr, "Function calls: %llu\n", (unsigned long long) vm.function_calls);
        fprintf(stderr, "Memory allocations: %llu\n", (unsigned long long) vm.memory_allocations);
        fprintf(stderr, "Stack max depth: %zu\n", vm.stack_max_depth);
    }
    insts_free(&insts);
    vm_free(&vm);
    return rc;
}

int main(int argc, char **argv) {
    FILE *f = fopen("../avm_bytecode.bin", "rb");
    if (!f) {
        perror("fopen");
        return 1;
    }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc(sz);
    if (!buf) {
        fprintf(stderr, "OOM\n");
        return 1;
    }
    fread(buf, 1, sz, f);
    fclose(f);
    VMPValue ret;
    int rc = vmp_run_bytecode(buf, (size_t) sz, 1, &ret);
    free(buf);
    fprintf(stderr, "vm rc=%d, ret.tag=%d\n", rc, (int) ret.tag);
    if (ret.tag == VT_I32) {
        fprintf(stderr, "\tret.i32=%d\n", ret.v.i32);
    } else if (ret.tag == VT_I64) {
        fprintf(stderr, "\tret.i64=%lld\n", (long long) ret.v.i64);
    } else if (ret.tag == VT_F32) {
        fprintf(stderr, "\tret.f32=%f\n", ret.v.f32);
    } else if (ret.tag == VT_F64) {
        fprintf(stderr, "\tret.f64=%lf\n", ret.v.f64);
    } else if (ret.tag == VT_Ptr) {
        fprintf(stderr, "\tret.ptr=0x%llx\n", (unsigned long long) ret.v.ptr);
    }
    return rc ? 1 : 0;
}

