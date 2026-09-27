# LLVM IR MBA 区域设计

MBA pass 把同位宽的 `add/sub/and/or/xor/select/phi` 连接成 SSA 区域，让整数状态以另一种表示穿过分支和循环。默认启用的跨类型路径使用 binary64 尾数；不适合浮点的指令使用有预算的整数变换。

## 配置和范围

```toml
[mba]
enable = true
float_regions = true
pre_expand = true
opaque_guard = true
max_instructions = 128
max_added_instructions = 2048
```

对应环境变量为 `AMICE_MBA`、`AMICE_MBA_FLOAT_REGIONS`、`AMICE_MBA_PRE_EXPAND`、`AMICE_MBA_OPAQUE_GUARD`、`AMICE_MBA_MAX_INSTRUCTIONS` 和 `AMICE_MBA_MAX_ADDED_INSTRUCTIONS`。函数注解可覆盖全部六项：

```c
__attribute__((annotate("+mba -mba_float_regions mba_max_instructions=32")))
unsigned sensitive(unsigned x, unsigned y);
```

`max_instructions` 统计每函数准入的原始指令，硬上限为 512；`max_added_instructions` 使用保守的生成指令估算，包含输入编码、phi 边转换、结果解码。预算为零时不改写。未采用的候选也可能占用预留预算，因此实际生成量可以更低。

组合路径为每条原始二元指令预留 96 条生成指令，未展开的浮点二元运算预留 24 条，整数回退及 select 预留 20 条，phi 为 `2 + 3 * 入边数`。全局载体设置另预留 12 条。展开不递归，最多五个虚拟运算节点；源指令预算始终按原始 IR 计算。预算不足、没有浮点区域、禁用 MBA 或禁用 `opaque_guard` 时不会创建种子全局变量。

浮点区域覆盖 `i1` 到 `i32`，仅在模块 triple 为 x86_64 或 aarch64、函数允许硬件浮点时启用。`strictfp`、`noimplicitfloat`、`naked`、`use-soft-float=true` 及禁用基础浮点指令的 target features 会阻止该路径。未知 triple 使用整数路径。其他 scalar integer 位宽到 `i128` 使用整数路径；向量和更宽整数保持原样。

当前不会改写乘除法、移位或 intrinsics，也不单独隐藏任意常量；它们可以成为区域边界。浮点 FMA 乘法残差和延迟范围恢复尚未纳入此版本。

旧的 auxiliary、rewrite depth、fixStack、optnone 配置及 `linearmba` 别名已移除。新的 `opaque_guard` 控制私有种子全局变量；不生成辅助栈变量，也不添加 optnone。已改写函数带有 `"amice.mba.done"="1"`，避免重复注册或重复优化流水线再次膨胀。

## 整数等价图与全局载体

`pre_expand` 默认开启：先把原始二元运算展开为整数环上的等价 DAG，再将每个节点直接发射成载体运算，中间不会产生一轮可被 InstCombine 提前化简的整数 IR。例如 `x+y = (x^y)+(x&y)+(x&y)`，`x&y = (x+y)-(x|y)`。每类 add/sub/and/or/xor 有两个编译时随机选择的变体，全部按原位宽取模，并在每个载体算术节点后恢复范围。输入只编码、freeze 一次，重复项共享该值；phi/select 仍连接整个区域。布尔运算的等价图也含算术，因此开启展开时纯布尔组件也可进入浮点区域。

`pre_expand` 仅作用于可用浮点区域的 i1–i32，其他路径继续使用整数回退。`-mba_pre_expand` 可以单独关闭展开，`-mba_opaque_guard` 可以关闭全局读取并使用编译时入口种子；两种模式都保留内部偏移更新。

`opaque_guard` 默认开启：每个实际改写的浮点函数拥有一个私有 i32 种子全局变量，调用入口执行一次 volatile load。种子生成本次调用共享的浮点指数及入口偏移，所有 32 位初始位模式均合法。pass 不写入种子，不依赖线程间变化。此读取可抑制常量传播，不保证所有等价结构都无法化简。

区域内部不再共享一个固定偏移。每个虚拟运算节点把前驱编码的头部、两个输入的数据及该节点的编译时随机盐混合，生成新的有界尾数偏移。phi/select 携带各自实际选中值的完整编码，所以循环状态可以随数据和前驱状态变化。它不是加密哈希，不保证每轮都产生不同偏移；掩码、位宽和移位次数等整数常量仍是编码的一部分。

加入 volatile 读取后，会保守清除当前模块中函数定义和非 intrinsic 调用点的旧 memory/readnone/readonly/writeonly、speculatable、nosync 摘要，覆盖直接调用、别名、间接调用和传递调用者。后续 LLVM 分析可以重新推导有效属性。模块外已经发生的常量折叠不会被恢复。只有发生全局载体改写时才执行这项失效处理。

## 尾数表示和语义

设原始整数位宽为 `w <= 32`。本次调用选择无偏指数 `e = ((seed >> 17) & 255) - 128`，并令指数位模式 `B = (e + 1023) << 52`。入口偏移为 `G = k * 2^33`，其中 `k = (seed & 0x1ffff) + 1`；后续每个节点生成自己的 k，始终满足 `1 <= k <= 2^17`。禁用全局载体时在编译时随机选择入口种子，内部偏移仍随数据计算。编码为：

```
E(x) = B | G | zext_i64(x)
bitcast_double(E(x)) = 2^e + (G + unsigned(x)) * 2^(e-52)
decode(E(x)) = trunc_iw(E(x))
```

所有有效编码值都是正常有限数。`G` 的低 33 位为零，低 `w` 位始终表示原整数。无偏指数覆盖 -128 到 127，最小非零尾数单位为 `2^-180`，不会成为次正规数。

对于加减法，先清空编码右操作数的低 w 位，提取它自己的浮点基准，然后从右操作数减去这个基准，得到精确的 `unsigned(y) * 2^(e-52)`。从编码左操作数加减该量后，结果仍处于 `[2^e, 2^(e+1))` 的同一个 binade 内。所有中间结果都是同一尾数单位的精确整数倍。最后保留位模式低 w 位，再补回新的 `B | G`，实现模 `2^w` 运算。右侧基准从实际 SSA 值导出，不再引用统一的函数级浮点常数。

每个虚拟节点以输入编码 X、Y 和编译时盐 salt 计算新的偏移：

```text
k_next = (((((X >> 33) XOR X) + Y) XOR salt) & 0x1ffff) + 1
G_next = k_next << 33
```

这里的加法按 i64 取模，不带溢出标志。混合使用前驱头部及当前数据，范围掩码确保偏移更新不会影响精度保证。区域外输入先 freeze 并编码，区域内直接传递编码状态。

这一范围同时支持最大值加法和借位减法。每次算术后恢复范围，因此循环迭代次数不消耗额外精度预算。四种 IEEE 舍入模式下结果相同，正常输入不产生浮点异常。没有 `fptosi/fptoui` 的越界转换，也不依赖 NaN、异常标志或 fast-math。

AND、OR、XOR 对两个编码操作数进行同类位运算，然后保留低 w 位并补回新头部，不能假定两边偏移相同。select 和 phi 在两条路径之间选择完整编码值。对 phi 先创建占位节点，再生成其他值，最后连接回边，以保持循环和不按支配顺序排列的基本块。

LLVM 要求来自同一前驱的重复 switch 边使用同一 phi 值，边编码因此复用。invoke/callbr 结果所在的 phi 和 EH pad 所在块的 phi 保留为整数边界，避免把编码插入非法位置。没有入边的空 phi 也保留为整数边界，在使用处 freeze 后再编码，避免引入范围未知的载体。原始值统一替换后再删除，内部无用解码交给正常 DCE 清理。

新增整数运算不继承 `nsw/nuw/exact`，浮点运算不添加 fast-math。源中由溢出标志导致的 poison 可以被精化为定义值；对于源中有定义的输入，必须保留输出。select 未选择分支中的 poison 不应被引入到最终结果。

区域外输入及 select 条件在进入浮点表示前先 freeze，以保证即使原来的 poison 最终被另一个 select 屏蔽，提前执行的浮点运算也只接收有效的有限载体。仅验证最终整数结果不足以发现这类问题，因此差分运行器同时检查浮点异常标志。

## 整数路径

加减法拆成高低两段，显式传递低段进位或借位，再合并结果。拆分点为位宽的一半，覆盖奇数位宽；`i1` 先扩展到 `i8`，最后截断。AND、OR、XOR 使用小型模整数恒等式。被重复引用的输入先 freeze 一次，防止 undef 在不同项或高低段中取不同值，破坏如 `undef & 0 = 0` 的约束。此路径不引入浮点，成本有限，但部分布尔恒等式可能被 LLVM 恢复，应视为覆盖和语义保障，而非新的抗化简能力。

`freeze` 的值稳定性遵循 [LLVM LangRef](https://llvm.org/docs/LangRef.html#freeze-instruction)。

## 验证和能力边界

配置完整 LLVM 开发工具链后，先构建新插件，避免测试辅助代码复用旧动态库：

```text
cargo build --release -p amice
cargo test --release -p amice --lib every_identity
cargo test --release -p amice --test mba --test mba_regions --test rust_mba -- --test-threads=1
```

工具链前缀按项目构建说明设置，例如 `LLVM_SYS_211_PREFIX`。Windows 构建还需按加载程序选择项目的链接 feature。

`mba_regions` 将原始 IR 单独编译成 reference 函数，并对变换后的函数做差分执行。主矩阵覆盖全部 `i1`–`i128` 位宽：`i1`–`i8` 穷举操作数对，其余位宽使用极值、逐位进借位和固定种子的随机输入，分别检查宽整数结果的两半。浮点与整数路径均经过 O0 和连续两轮 O2；较小的矩阵另外覆盖 O1、O3、Os、Oz、LTO 以及先优化再混淆。产物位于 `target/test-outputs/mba-regions/`。

其他回归覆盖以下边界：

- 相互依赖的循环 phi、重复 switch 边、不可归约控制流、区域预算截断、反序排列基本块和 512 条准入上限。
- 被 select 屏蔽的 poison、undef、合法的 nsw/nuw 输入、volatile/atomic 内存和 intrinsic 边界。
- 四种舍入模式、原有浮点异常标志的保留、strict/soft-float/noimplicitfloat 和禁用基础浮点指令的函数属性。
- C++ 异常与析构的实际执行、Windows EH funclet 的 IR 和目标码验证、asm goto 的 callbr 输出。
- 向量、超过 128 位的整数、不支持的操作和 target triple；x86、ARM、RISC-V、WebAssembly 等目标的对象文件生成。
- 缺失 section、空或全零字节数组、无终止零、内嵌零和非 UTF-8 注解；文件名字段不作为配置读取。
- 所有等价式变体的小位宽穷举、展开/载体开关组合、种子全零/边界/全一、O3/LTO 后的载体保留、多线程调用和过期调用者内存属性。
- 全部 256 档指数、入口偏移极值、动态种子和实际生成的载体随输入变化；在 O0 和 O3/LTO 后检查结果、范围、舍入模式和异常标志。展开循环另外检查浮点算术使用 SSA 操作数，避免重新引入统一的浮点立即数。

Linux CI 的 LLVM 21/22 构建会执行 MBA 回归。跨平台运行可使用 `scripts/test_mba_cross_runtime.py` 将上述主矩阵编译成 Windows x64 或 Android arm64 的独立可执行文件。例如，在生成主矩阵产物后执行：

```text
python3 scripts/test_mba_cross_runtime.py --llvm-bin /usr/lib/llvm-21/bin --plugin target/release/libamice.so --fixtures target/test-outputs/mba-regions --out target/mba-windows --target windows
python3 scripts/test_mba_cross_runtime.py --llvm-bin /usr/lib/llvm-21/bin --plugin target/release/libamice.so --fixtures target/test-outputs/mba-regions --out target/mba-android --target android
```

插件必须与 `--llvm-bin` 对应，目录参数从当前工作目录解析。脚本不需要目标 SDK/sysroot，也不自动部署；将生成的四个 `mba-*` 可执行文件放到对应目标机运行。每个程序分别关闭、开启 FTZ（Windows 同时切换 DAZ），并在每种状态下检查四种舍入模式与异常标志。这个独立运行器验证生成代码的目标 ABI、整数结果和浮点环境；Windows 插件 DLL 的加载、Android Bionic 动态链接及完整应用集成需要另行测试。其他架构的对象文件生成也不等同于真机运行。

保留浮点 IR 只说明没有被这条 LLVM 优化流水线恢复，不等于抗逆向或抗合成证明。动态头部仍遵循已知的区域编码规则，可能被专门的跨类型分析识别和消除。后续强度评估应使用实际区域和机器码、可观测结果、攻击成功率及运行时成本；不能把 IR 增量或表达式工具不支持浮点当作保护强度。
