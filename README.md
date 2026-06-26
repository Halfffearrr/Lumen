# Lumen

Lumen 是一个使用 Rust 从零实现的小型动态脚本语言。项目没有使用解析器生成器，也没有依赖现成的虚拟机框架，而是手动完成了从源码到字节码执行的完整流程。

它的设计借鉴了一些现代语言特性，例如默认不可变变量、表达式风格的 `if` 和代码块、范围语法、闭包捕获，以及简洁的花括号语法。

```text
source .lm -> Lexer -> Tokens -> Parser -> AST -> Resolver
           -> Compiler -> Bytecode -> VM -> result
```

## 快速开始

在项目根目录执行：

```sh
cargo build
cargo test
cargo run -p lumen-cli -- examples/fizzbuzz.lm
cargo run -p lumen-cli
```

上面几条命令分别用于构建项目、运行测试、执行示例脚本，以及启动交互式 REPL。REPL 中可以直接输入 Lumen 代码，按 `Ctrl-D` 退出。

也可以先构建 release 版本，再直接运行生成的 `lumen` 可执行文件：

```sh
cargo build --release
./target/release/lumen examples/fib.lm
./target/release/lumen --disassemble examples/fizzbuzz.lm
./target/release/lumen --gc-demo
```

其中：

- `--disassemble` 用于查看编译后的字节码指令。
- `--gc-demo` 用于运行并发 mark-sweep GC 的演示。

## 语言示例

```rust
// 变量默认不可变；需要修改时使用 let mut。
let pi = 3.14
let mut count = 0

// if 是表达式，可以产生一个值。
let level = if count > 10 { "high" } else { "low" }

// 范围和 for 循环；.. 不包含右边界，..= 包含右边界。
for i in 1..=5 {
    count = count + i
}

// 字符串插值。
print("count is {count}, level {level}")

// 函数、递归，以及可以捕获外部变量的闭包。
fn make_counter() {
    let mut n = 0
    return fn() { n = n + 1; return n }
}

let next = make_counter()
print(next())   // 1
print(next())   // 2
```

更多示例可以查看 [`examples/`](examples/) 目录：

- `fizzbuzz.lm`：基础循环和条件判断。
- `tour.lm`：语言特性总览。
- `fib.lm`：递归函数。
- `closures.lm`：闭包和变量捕获。
- `calc.lm`：使用 Lumen 编写的简单计算器。
- `bench.lm`：简单性能测试示例。

## 已实现功能

- 基础值类型：`int`、`float`、`bool`、`nil`、`str`、`list`、`dict`、`range` 和函数。
- 默认不可变变量：通过 `let` 声明，使用 `let mut` 显式允许修改。
- 静态语义检查：未定义变量、重复绑定、重复参数、非法 `return`/`break`、不可变变量赋值、已知函数调用参数数量等。
- 表达式风格语法：`if`/`else` 和代码块都可以产生值。
- 控制流：`if`、`while`、`for ... in range/list`、`loop` 和 `break`。
- 一等函数：支持递归、匿名函数 `fn(x) { ... }` 和闭包捕获。
- 字符串插值：例如 `"hi {name}, {1 + 1}"`，并支持嵌套表达式。
- 标准库函数：`print`、`len`、`type`、`str`、`int`、`float`、`sqrt`、`abs`、`floor`、`min`、`max`、`push`、`pop`、`keys`、`values`、`error`、`clock`。
- 字节码虚拟机：源码会先编译为字节码，再由 VM 执行。
- GC 演示：运行时包含 mark-sweep 风格的对象管理演示。

## 项目结构

项目采用 Cargo workspace 组织，不同阶段被拆分到独立 crate 中。

| Crate | 主要职责 |
| --- | --- |
| `lumen-common` | 共享的源码位置 `Span`、诊断信息 `Diagnostic`、内置函数定义 |
| `lumen-lexer` | 将源码文本转换为 token 流 |
| `lumen-parser` | 将 token 转换为 AST，并配合 resolver 做静态检查 |
| `lumen-compiler` | 将 AST 编译为字节码 `Chunk`，并提供反汇编功能 |
| `lumen-vm` | 栈式虚拟机、运行时值、闭包和 mark-sweep GC |
| `lumen-cli` | 命令行入口，支持运行脚本、REPL、反汇编和 GC demo |

## 扩展与亮点

| 编号 | 功能 | 位置 |
| --- | --- | --- |
| 1 | 带源码位置的错误提示 | `lumen-cli` 的诊断渲染逻辑，结合各节点的 `Span` |
| 2 | REPL 交互模式 | 不带参数运行 `lumen` |
| 4 | 并发 mark-sweep GC 演示 | `lumen-vm/src/gc.rs`，通过 `lumen --gc-demo` 运行 |
| 5 | 标准库函数 | `lumen-vm/src/vm.rs` 中的 builtins |
| 6 | 静态检查和语法错误恢复 | `lumen-parser/src/resolver.rs`、`parser.rs` |
| 7 | 自举方向尝试 | `examples/calc.lm`，使用 Lumen 编写计算器 |
| 8 | 字节码反汇编器 | `lumen --disassemble` |

## 文档

- [`docs/grammar.md`](docs/grammar.md)：Lumen 语法说明。
- [`docs/bytecode.md`](docs/bytecode.md)：字节码指令集说明。
- [`docs/report.md`](docs/report.md)：项目设计说明。

## 构建、测试与代码检查

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets
```

如果希望把 clippy 警告作为错误处理，可以使用：

```sh
cargo clippy --all-targets -- -D warnings
```

## 常用演示命令

```sh
cargo run -p lumen-cli -- examples/fizzbuzz.lm
cargo run -p lumen-cli -- examples/closures.lm
cargo run -p lumen-cli -- --disassemble examples/fizzbuzz.lm
cargo run -p lumen-cli -- --gc-demo
cargo test
```

这些命令可以覆盖项目的主要演示点：脚本执行、闭包、字节码、GC demo 和自动化测试。
