# Rust 学习文档

> 面向有系统编程经验（如 Go/C）的工程师，从零到能写实际项目的 Rust 学习指南。
> 所有示例基于 Rust 2021 edition（stable 工具链即可运行）。

---

## 目录

1. [Rust 是什么，为什么学它](#1-rust-是什么为什么学它)
2. [环境搭建与工具链](#2-环境搭建与工具链)
3. [快速上手：第一个程序与 Cargo](#3-快速上手第一个程序与-cargo)
4. [基础语法：变量、类型、函数](#4-基础语法变量类型函数)
5. [控制流](#5-控制流)
6. [所有权系统（Rust 的核心）](#6-所有权系统rust-的核心)
7. [借用与引用](#7-借用与引用)
8. [结构体与枚举](#8-结构体与枚举)
9. [模式匹配](#9-模式匹配)
10. [集合类型](#10-集合类型)
11. [错误处理](#11-错误处理)
12. [泛型与 Trait](#12-泛型与-trait)
13. [生命周期](#13-生命周期)
14. [闭包与迭代器](#14-闭包与迭代器)
15. [智能指针](#15-智能指针)
16. [模块系统与项目组织](#16-模块系统与项目组织)
17. [并发编程](#17-并发编程)
18. [异步编程 async/await](#18-异步编程-asyncawait)
19. [测试与文档](#19-测试与文档)
20. [宏（入门）](#20-宏入门)
21. [Unsafe Rust（入门）](#21-unsafe-rust入门)
22. [实战项目建议](#22-实战项目建议)
23. [学习路线与资源](#23-学习路线与资源)
24. [常见编译错误与心法](#24-常见编译错误与心法)

---

## 1. Rust 是什么，为什么学它

Rust 是一门**系统级编程语言**，核心卖点是：

- **内存安全**：无 GC，靠编译期检查（所有权/借用系统）保证没有悬垂指针、use-after-free、数据竞争。
- **零成本抽象**：迭代器、泛型、async 等抽象不带来运行时开销（类似 C++ 模板，但更安全）。
- **无数据竞争**：`Send`/`Sync` trait 在编译期防止并发 bug。
- **现代工具链**：Cargo（构建+包管理+测试+文档一体），体验远好于 C/C++。

### 与 Go 的对比（帮助你建立心智模型）

| 维度 | Go | Rust |
|------|-----|------|
| 内存管理 | GC | 所有权（编译期，无运行时开销） |
| 并发模型 | goroutine + channel | 线程 + `Send/Sync`；async/await（tokio） |
| 错误处理 | `if err != nil` | `Result<T, E>` + `?` 运算符 |
| 空值 | `nil` 指针可 panic | 无 null，用 `Option<T>` |
| 泛型 | 1.18 后有，较简单 | 强大，编译期单态化 |
| 编译速度 | 快 | 慢（单态化+借用检查） |
| 学习曲线 | 平缓 | 陡峭（前 2~4 周最痛苦） |

一句话：**Rust 把很多运行时错误变成了编译期错误**，代价是和编译器搏斗的学习期。

---

## 2. 环境搭建与工具链

### 2.1 安装（rustup）

```bash
# 安装 rustup（Rust 官方版本管理器，类似 gvm/nvm）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 国内加速（可选）
export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup
```

安装完成后验证：

```bash
rustc --version     # 编译器
cargo --version     # 构建工具
rustup update       # 更新工具链
```

### 2.2 常用工具链概念

- **stable / nightly**：Rust 有 stable（稳定版）和 nightly（实验版）两条通道。学习用 stable 即可。
- **edition**：语言版本（2015/2018/2021/2024），新项目用 2021 或 2024。
- **rustup 常用命令**：

```bash
rustup toolchain install nightly   # 装 nightly
rustup default stable              # 切默认工具链
rustup component add clippy        # 代码检查工具（类似 lint）
rustup component add rustfmt       # 代码格式化
rustup doc                         # 打开离线文档
```

### 2.3 Cargo（核心工具，类比 go mod + make + npm）

```bash
cargo new hello          # 创建新项目
cargo new --lib mylib    # 创建库项目
cargo build              # 编译（debug 模式）
cargo build --release    # 编译（优化模式，性能测试必须用这个）
cargo run                # 编译并运行
cargo check              # 只检查不生成二进制（最快，开发时高频使用）
cargo test               # 运行测试
cargo clippy             # 静态检查（强烈建议养成习惯）
cargo fmt                # 格式化
cargo doc --open         # 生成并打开 API 文档
cargo add serde          # 添加依赖（类似 go get）
```

### 2.4 Cargo.toml 与依赖

```toml
[package]
name = "hello"
version = "0.1.0"
edition = "2021"

[dependencies]
# 指定版本 + 特性（feature）
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }

[dev-dependencies]   # 仅测试用依赖
criterion = "0.5"
```

> 国内镜像：在 `~/.cargo/config.toml` 配置 rsproxy 或 ustc 源可大幅加速依赖下载。

---

## 3. 快速上手：第一个程序与 Cargo

### 3.1 hello world

```rust
fn main() {
    println!("Hello, world!");  // println! 是宏，不是函数（感叹号是宏的标志）
}
```

### 3.2 项目结构

```
hello/
├── Cargo.toml        # 项目配置（类似 go.mod）
├── Cargo.lock        # 依赖锁定（类似 go.sum，自动生成）
└── src/
    ├── main.rs       # 二进制入口
    └── lib.rs        # 库入口（库项目才有）
```

### 3.3 带参数的程序（读命令行）

```rust
use std::env;       // 类似 Go 的 import，引入标准库模块

fn main() {
    let args: Vec<String> = env::args().collect();
    // args[0] 是程序名，args[1..] 是参数
    if args.len() < 2 {
        eprintln!("usage: {} <name>", args[0]);
        std::process::exit(1);
    }
    println!("hello, {}", args[1]);
}
```

---

## 4. 基础语法：变量、类型、函数

### 4.1 变量与可变性

**Rust 变量默认不可变**——这是和几乎所有语言不同的第一课：

```rust
fn main() {
    let x = 5;        // 不可变绑定
    // x = 6;         // 编译错误！cannot assign twice to immutable variable

    let mut y = 5;    // mut 显式声明可变
    y = 6;            // OK

    const MAX: u32 = 100_000;  // 常量：必须标注类型，编译期确定
}
```

**变量遮蔽（shadowing）**：同名新绑定覆盖旧绑定，类型都可以换：

```rust
let spaces = "   ";        // &str
let spaces = spaces.len(); // usize，合法！这不是 mut，是新变量
```

### 4.2 基本类型

```rust
fn main() {
    // ---- 整数 ----
    let a: i32 = -42;      // i8/i16/i32/i64/i128/isize
    let b: u64 = 42;       // u8/u16/u32/u64/u128/usize
    let c = 1_000_000;     // 下划线分隔可读性
    let hex = 0xff;
    let bin = 0b1010;

    // 整数溢出：debug 模式 panic；release 模式回绕（同 C）
    // 显式处理溢出：
    let d: u8 = 255;
    let e = d.checked_add(1);      // Option<u8>，None 表示溢出
    let f = d.saturating_add(1);   // 饱和到 255
    let g = d.wrapping_add(1);     // 回绕到 0

    // ---- 浮点 ----
    let x = 2.5_f64;   // f32/f64，默认 f64

    // ---- 布尔与字符 ----
    let flag: bool = true;
    let ch: char = '中';   // char 是 4 字节 Unicode 标量值

    // ---- 类型转换：必须显式 as ----
    let i: i32 = 100;
    let j: i64 = i as i64;   // 不像 Go 有自动提升，也不允许隐式转换
}
```

### 4.3 复合类型：元组与数组

```rust
fn main() {
    // 元组：固定长度，元素类型可不同
    let tup: (i32, f64, char) = (500, 6.4, 'z');
    let (a, b, c) = tup;               // 解构
    println!("{} {}", tup.0, tup.1);   // 按索引访问

    // 数组：固定长度，同类型，栈上分配
    let arr: [i32; 5] = [1, 2, 3, 4, 5];
    let zeros = [0; 10];               // 10 个 0
    println!("len={}, first={}", arr.len(), arr[0]);
    // arr[10] 会 panic（越界检查），不像 C 是未定义行为
}
```

### 4.4 字符串初步：`String` vs `&str`（高频困惑点）

```rust
fn main() {
    let s1: String = String::from("hello");  // 堆分配，可增长，拥有所有权
    let s2: &str = "world";                  // 字符串字面量/切片，固定，借用

    // &str → String
    let s3 = s2.to_string();
    let s4 = String::from(s2);

    // String → &str（解引用）
    let s5: &str = &s1;

    // 拼接
    let s6 = format!("{} {}", s1, s2);  // 推荐，类似 fmt.Sprintf
    let s7 = s1 + " " + s2;             // 会消耗 s1（所有权问题，见第 6 节）
}
```

**经验法则**：函数参数用 `&str`（更通用），需要拥有/存储时用 `String`。

### 4.5 函数

```rust
// 参数必须标注类型；返回类型用 ->
fn add(a: i32, b: i32) -> i32 {
    a + b    // 注意：没有分号！这是表达式，直接作为返回值
}

// 显式 return 也可以，但 Rust 惯例用表达式
fn add2(a: i32, b: i32) -> i32 {
    return a + b;
}

// 多返回值：用元组
fn swap(a: i32, b: i32) -> (i32, i32) {
    (b, a)
}

// 表达式 vs 语句的区别（重要！）
fn demo() {
    let y = {
        let x = 3;
        x + 1        // 表达式：有值；加分号就变成语句，值为 ()
    };               // y = 4
    println!("{y}");
}

fn main() {
    println!("{}", add(1, 2));
    let (a, b) = swap(1, 2);
    demo();
}
```

**核心概念**：Rust 是**基于表达式**的语言。`if`、`match`、`{}` 块都是表达式，有值。函数体最后一个表达式就是返回值（**不要加分号**，加了分号类型变成 `()`，这是新手最常见的编译错误之一）。

---

## 5. 控制流

### 5.1 if 是表达式

```rust
fn main() {
    let n = 6;

    // if-else
    if n > 5 {
        println!("big");
    } else {
        println!("small");
    }

    // if 作为表达式赋值（类似三元运算符，但更强大）
    let x = if n > 5 { "big" } else { "small" };
    // 注意：两个分支类型必须一致
}
```

### 5.2 loop / while / for

```rust
fn main() {
    // loop：无限循环，可以从循环中返回值
    let mut count = 0;
    let result = loop {
        count += 1;
        if count == 10 {
            break count * 2;   // break 带值 → loop 的返回值
        }
    };

    // 标签 + break/continue（跳出多层循环）
    'outer: for i in 0..10 {
        for j in 0..10 {
            if i * j > 50 {
                break 'outer;
            }
        }
    }

    // while
    let mut n = 3;
    while n != 0 {
        n -= 1;
    }

    // for：遍历迭代器（Rust 中最常用）
    for i in 0..5 {}          // 0,1,2,3,4（左闭右开）
    for i in 0..=5 {}         // 0..5 含 5（闭区间）
    for i in (0..5).rev() {}  // 倒序

    let arr = [10, 20, 30];
    for v in arr {}           // 获得所有权（数组较小，拷贝语义）
    for v in &arr {}          // 借用（引用遍历），最常用
    for v in &mut arr {}      // 可变借用
    for (i, v) in arr.iter().enumerate() {  // 带索引
        println!("{i}: {v}");
    }
}
```

> Go 里 `for i, v := range arr` 的对应物就是 `for (i, v) in xxx.iter().enumerate()`。

---

## 6. 所有权系统（Rust 的核心）

这是 Rust 最重要的概念，**理解了所有权，Rust 就通了 80%**。

### 6.1 为什么需要所有权

- C/C++：手动管理内存 → use-after-free、内存泄漏、double free。
- Java/Go：GC → 运行时开销、STW、不确定性延迟。
- Rust：**编译期**通过一套规则确定每块内存的生命周期，零运行时开销。

### 6.2 三条规则（背下来）

1. Rust 中每个值都有一个**所有者（owner）**。
2. 同一时刻，**只能有一个所有者**。
3. 所有者离开作用域，值被**自动释放**（调用 `drop`）。

```rust
fn main() {
    {                       // 作用域开始
        let s = String::from("hello");  // s 拥有这个字符串
        // 使用 s ...
    }                       // 作用域结束，s 的内存自动释放（不需要 free）
}
```

### 6.3 移动（Move）语义

```rust
fn main() {
    let s1 = String::from("hello");
    let s2 = s1;             // 所有权从 s1 移动到 s2！
    // println!("{}", s1);   // 编译错误！s1 已失效（防止 double free）

    // 对比：基本类型是拷贝（Copy 语义）
    let x = 5;
    let y = x;               // 拷贝，x 依然可用（i32 在栈上，拷贝廉价且安全）
    println!("{}", x);       // OK
}
```

**规则**：实现了 `Copy` trait 的类型（整数、浮点、bool、char、不可变引用、由 Copy 类型组成的元组/数组）赋值时是拷贝；其他类型（`String`、`Vec`、自定义结构体等堆类型）是**移动**。

### 6.4 函数与所有权

```rust
fn take_ownership(s: String) {   // s 获得所有权
    println!("{s}");
}                                 // s 在这里被 drop

fn give_back() -> String {
    String::from("hello")         // 返回值把所有权移出去
}

fn main() {
    let s = String::from("hello");
    take_ownership(s);
    // println!("{s}");           // 错误！s 的所有权已交出去

    let s2 = give_back();         // s2 接收所有权
    println!("{s2}");
}
```

### 6.5 克隆：想要拷贝怎么办

```rust
fn main() {
    let s1 = String::from("hello");
    let s2 = s1.clone();      // 显式深拷贝
    println!("{s1} {s2}");    // 都可用
}
```

> Go 里 `b := a` 对 slice/map 只是拷贝头，Rust 里赋值即移动。**从 Go 迁移过来最容易踩的坑就是把赋值当拷贝用。**

---

## 7. 借用与引用

每次传参都交出所有权太麻烦。**借用（borrowing）**让我们使用值而不获取所有权。

### 7.1 不可变引用 `&T`

```rust
fn calc_len(s: &String) -> usize {   // s 是引用，不拥有值
    s.len()
}   // s 离开作用域，但它不拥有值，所以什么都不释放

fn main() {
    let s = String::from("hello");
    let len = calc_len(&s);   // &s 创建引用传进去，所有权保留在 s
    println!("{s} len={len}"); // s 依然可用！
}
```

### 7.2 可变引用 `&mut T`

```rust
fn push_world(s: &mut String) {
    s.push_str(", world");
}

fn main() {
    let mut s = String::from("hello");
    push_world(&mut s);
    println!("{s}");
}
```

### 7.3 借用规则（编译器强制的核心规则）

1. **任意多个不可变引用 `&T`** 或 **恰好一个可变引用 `&mut T`**，二者不能同时存在。
2. 引用必须**始终有效**（不允许悬垂引用）。

```rust
fn main() {
    let mut s = String::from("hello");

    let r1 = &s;
    let r2 = &s;          // 多个不可变引用 OK
    println!("{r1} {r2}");
    // r1、r2 在这之后不再使用 → 生命周期结束

    let r3 = &mut s;      // OK（NLL：非词法作用域，r1/r2 已死）
    r3.push_str("!");

    // let r4 = &s; let r5 = &mut s;  // 错误！不可变引用存活期间不能有可变引用
}
```

**为什么这样设计**：防止数据竞争。只要没有"同时读写"或"同时写写"，就不会有竞争。这是 Rust 在编译期消灭数据竞争的根基。

### 7.4 悬垂引用检查

```rust
// 编译错误！返回局部变量的引用
// fn dangle() -> &String {
//     let s = String::from("hello");
//     &s   // s 在函数结束时释放，引用会悬垂
// }

// 正确：返回所有权
fn no_dangle() -> String {
    let s = String::from("hello");
    s
}
```

### 7.5 切片（Slice）：借用集合的一部分

```rust
fn main() {
    let s = String::from("hello world");
    let hello: &str = &s[0..5];    // 字符串切片：借用，不拥有
    let world: &str = &s[6..];
    let whole: &str = &s[..];      // 简写

    let v = vec![1, 2, 3, 4, 5];
    let sub: &[i32] = &v[1..3];    // &[2, 3]
}
```

**实践总结（参数类型选择）**：

```rust
fn good(s: &str) {}          // 推荐：&str 同时接受 String 和字面量
fn bad(s: &String) {}        // 过度限制
fn good2(v: &[i32]) {}       // 推荐
fn bad2(v: &Vec<i32>) {}     // 过度限制
```

---

## 8. 结构体与枚举

### 8.1 结构体

```rust
// 定义
struct User {
    username: String,
    email: String,
    active: bool,
    login_count: u64,
}

// 元组结构体（字段无名）
struct Point(f64, f64);

// 单元结构体（无字段，常用于 trait 标记）
struct Marker;

fn main() {
    // 创建
    let mut u = User {
        username: String::from("yuan"),
        email: String::from("y@x.com"),
        active: true,
        login_count: 1,
    };
    u.login_count += 1;   // 整个实例可变，或全部不可变（没有字段级可变性）

    // 字段初始化简写：变量名与字段名相同
    let username = String::from("bob");
    let email = String::from("b@x.com");
    let u2 = User { username, email, active: false, login_count: 0 };

    // 结构体更新语法（注意：move！其余字段的所有权移给 u3）
    let u3 = User { email: String::from("c@x.com"), ..u2 };
    // println!("{}", u2.username);  // 错误！username 被 move 走了

    // 方法语法
    impl User {
        // 关联函数（无 self），类似静态方法/构造函数，通常用 new 命名
        fn new(username: String, email: String) -> Self {
            Self { username, email, active: true, login_count: 0 }
        }

        // 方法：&self 借用（最常用）
        fn is_active(&self) -> bool {
            self.active
        }

        // 可变方法
        fn login(&mut self) {
            self.login_count += 1;
        }

        // 消耗所有权的方法（少见，调用后原值不可用）
        fn destroy(self) -> String {
            self.username
        }
    }

    let mut u = User::new(String::from("yuan"), String::from("y@x.com"));
    u.login();
    println!("{}", u.is_active());
}
```

### 8.2 枚举（Rust 的枚举远强于 Go 的 iota）

**每个变体可以携带不同类型的数据** —— 这是代数数据类型（ADT）：

```rust
enum IpAddr {
    V4(u8, u8, u8, u8),
    V6(String),
}

// 标准库最重要的两个枚举：
// enum Option<T> { Some(T), None }        // 替代 null
// enum Result<T, E> { Ok(T), Err(E) }     // 替代 error

enum Shape {
    Circle { radius: f64 },
    Rectangle { w: f64, h: f64 },
    Triangle(f64, f64, f64),
}

impl Shape {
    fn area(&self) -> f64 {
        match self {   // match 强制穷尽所有变体，漏一个就是编译错误！
            Shape::Circle { radius } => std::f64::consts::PI * radius * radius,
            Shape::Rectangle { w, h } => w * h,
            Shape::Triangle(a, b, c) => {
                // 海伦公式示意
                let s = (a + b + c) / 2.0;
                (s * (s - a) * (s - b) * (s - c)).sqrt()
            }
        }
    }
}

fn main() {
    let ip = IpAddr::V4(192, 168, 1, 1);
    let c = Shape::Circle { radius: 2.0 };
    println!("{}", c.area());
}
```

### 8.3 Option<T>：没有 null 的世界

```rust
fn find_user(id: u32) -> Option<String> {
    if id == 1 {
        Some(String::from("yuan"))
    } else {
        None
    }
}

fn main() {
    let r = find_user(1);

    // 方式1：match
    match r {
        Some(name) => println!("found: {name}"),
        None => println!("not found"),
    }

    // 方式2：if let（只关心一种情况）
    if let Some(name) = find_user(1) {
        println!("found: {name}");
    }

    // 方式3：组合器方法
    let name = find_user(1).unwrap_or("guest".to_string());
    let len = find_user(1).map(|n| n.len());          // Option<usize>
    let r2 = find_user(1).and_then(|n| Some(n.len())); // 链式处理

    // unwrap/expect：有值取值，None 直接 panic（原型代码可用，生产慎用）
    let name = find_user(1).unwrap();
    let name = find_user(1).expect("user must exist");
}
```

---

## 9. 模式匹配

`match` 是 Rust 的瑞士军刀，可以匹配几乎一切：

```rust
enum Message {
    Quit,
    Move { x: i32, y: i32 },
    Write(String),
    ChangeColor(u8, u8, u8),
}

fn process(msg: Message) {
    match msg {
        Message::Quit => println!("quit"),
        Message::Move { x, y } => println!("move to ({x},{y})"),
        Message::Write(text) => println!("write: {text}"),
        Message::ChangeColor(r, g, b) => println!("color ({r},{g},{b})"),
    }
}

fn main() {
    // 匹配字面量与范围
    let n = 5;
    match n {
        1 => println!("one"),
        2..=5 => println!("2-5"),          // 范围匹配
        6 | 7 => println!("6 or 7"),       // 或
        _ => println!("other"),            // _ 通配符（必须穷尽）
    }

    // 解构元组
    let point = (0, 7);
    match point {
        (0, y) => println!("on y axis, y={y}"),
        (x, 0) => println!("on x axis, x={x}"),
        (x, y) => println!("({x},{y})"),
    }

    // 匹配守卫（额外条件）
    let n = Some(4);
    match n {
        Some(x) if x > 3 => println!("big: {x}"),
        Some(x) => println!("small: {x}"),
        None => {}
    }

    // 绑定：@ 在匹配的同时绑定值
    let n = 5;
    match n {
        x @ 1..=5 => println!("in range: {x}"),
        _ => {}
    }

    // match 必须穷尽：编译器会报错 "non-exhaustive patterns"
}
```

**if let / while let / let-else**：

```rust
fn main() {
    let opt: Option<i32> = Some(3);

    // if let：单模式匹配，比 match 简洁
    if let Some(x) = opt {
        println!("{x}");
    }

    // let-else：模式不匹配就走 else（提前返回），Rust 1.65+
    let Some(x) = opt else {
        return;
    };
    println!("{x}");

    // while let：循环匹配
    let mut stack = vec![1, 2, 3];
    while let Some(top) = stack.pop() {
        println!("{top}");
    }
}
```

---

## 10. 集合类型

### 10.1 Vec<T>（动态数组，对应 Go 的 slice）

```rust
fn main() {
    // 创建
    let mut v: Vec<i32> = Vec::new();
    v.push(1);
    v.push(2);
    let v2 = vec![1, 2, 3];                 // 宏简写
    let v3 = vec![0; 10];                   // 10 个 0（类似 make([]int, 10)）

    // 访问：两种方式
    let third: &i32 = &v2[2];               // 越界 → panic
    let third: Option<&i32> = v2.get(2);    // 越界 → None（推荐对外部输入用）

    // 遍历
    for x in &v2 { println!("{x}"); }

    // 可变遍历
    let mut v4 = vec![1, 2, 3];
    for x in &mut v4 { *x *= 2; }           // *x 解引用修改

    // 常用操作
    let len = v2.len();
    let has = v2.contains(&2);
    let popped = v4.pop();                  // Option<i32>
    let sum: i32 = v2.iter().sum();
    let evens: Vec<i32> = v2.iter().filter(|x| *x % 2 == 0).copied().collect();

    // 注意：借用期间不能修改（所有权规则的体现）
    let mut v = vec![1, 2, 3];
    let first = &v[0];
    // v.push(4);   // 错误！first 借用期间不能可变借用（push 可能导致扩容，使引用失效）
    println!("{first}");
    v.push(4);      // OK，first 已不再使用
}
```

### 10.2 HashMap<K, V>

```rust
use std::collections::HashMap;

fn main() {
    let mut scores: HashMap<String, i32> = HashMap::new();

    // 插入（key/value 的所有权移入 map）
    scores.insert(String::from("yuan"), 95);

    // 访问
    let s = scores.get("yuan");            // Option<&i32>
    if let Some(s) = scores.get("yuan") {
        println!("{s}");
    }

    // entry API：不存在才插入（Go 里要先判断）
    scores.entry("bob".to_string()).or_insert(60);

    // entry 统计词频（经典用法）
    let text = "hello world hello rust";
    let mut counter: HashMap<&str, i32> = HashMap::new();
    for word in text.split_whitespace() {
        *counter.entry(word).or_insert(0) += 1;
    }
    println!("{:?}", counter);   // Debug 打印

    // 遍历
    for (k, v) in &scores {
        println!("{k}: {v}");
    }
}
```

### 10.3 其他常用集合

```rust
use std::collections::{HashSet, BTreeMap, VecDeque, BinaryHeap};

fn main() {
    // HashSet：去重、集合运算
    let a: HashSet<i32> = [1, 2, 3].into();
    let b: HashSet<i32> = [2, 3, 4].into();
    let inter: HashSet<_> = a.intersection(&b).copied().collect();  // {2,3}

    // BTreeMap：有序 map（按键排序），HashMap 无序
    let mut m = BTreeMap::new();
    m.insert(3, "c");
    m.insert(1, "a");

    // VecDeque：双端队列（对应 Go 没有的东西，做队列用）
    let mut dq = VecDeque::new();
    dq.push_back(1);
    dq.push_front(0);
    dq.pop_front();

    // BinaryHeap：最大堆
    let mut heap = BinaryHeap::new();
    heap.push(3);
    heap.push(1);
    let max = heap.pop();   // Some(3)
}
```

**选择指南**：
- 数组/列表 → `Vec<T>`
- 键值查找 → `HashMap`（默认）/ `BTreeMap`（要有序）
- 去重 → `HashSet` / `BTreeSet`
- 队列 → `VecDeque`
- 并发队列 → `crossbeam` / `tokio::sync::mpsc`

---

## 11. 错误处理

Rust 没有 exception（panic 只用于不可恢复错误），核心是 `Result<T, E>`。

### 11.1 Result 基础

```rust
use std::fs::File;
use std::io::{self, Read};

// 返回 Result：Ok(值) 或 Err(错误)
fn read_username(path: &str) -> Result<String, io::Error> {
    let mut s = String::new();
    File::open(path)?.read_to_string(&mut s)?;  // ? 运算符：见下文
    Ok(s.trim().to_string())
}

fn main() {
    match read_username("/tmp/user") {
        Ok(name) => println!("user: {name}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
```

### 11.2 `?` 运算符（错误传播的语法糖）

```rust
// 上面等价于：
fn read_username2(path: &str) -> Result<String, io::Error> {
    let mut s = String::new();
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => return Err(e),   // ? 就是干这个的
    };
    match f.read_to_string(&mut s) {
        Ok(_) => Ok(s.trim().to_string()),
        Err(e) => Err(e),
    }
}
```

**`?` 规则**：遇到 `Err` 立即从函数返回（错误值会被 `From` 转换），遇到 `Ok` 解包继续。**只能在返回 `Result`（或 `Option`）的函数中使用。**

### 11.3 自定义错误类型

```rust
use std::fmt;
use std::error::Error;

#[derive(Debug)]
enum AppError {
    NotFound(String),
    Parse(std::num::ParseIntError),
    Custom(String),
}

// 实现 Display 以便打印
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AppError::NotFound(key) => write!(f, "not found: {key}"),
            AppError::Parse(e) => write!(f, "parse error: {e}"),
            AppError::Custom(msg) => write!(f, "{msg}"),
        }
    }
}

impl Error for AppError {}

// From 转换：让 ? 能自动把底层错误转成 AppError
impl From<std::num::ParseIntError> for AppError {
    fn from(e: std::num::ParseIntError) -> Self {
        AppError::Parse(e)
    }
}

fn parse_config(s: &str) -> Result<i32, AppError> {
    let n: i32 = s.trim().parse()?;   // ParseIntError 自动转 AppError
    Ok(n)
}
```

### 11.4 实际项目：用 thiserror / anyhow

手写 Display/From 太啰嗦，实际项目用两个流行的 crate：

```rust
// 库（library）：用 thiserror，定义精确的错误类型
use thiserror::Error;

#[derive(Error, Debug)]
enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("parse error")]
    Parse(#[from] std::num::ParseIntError),  // 自动生成 From
}

// 应用（binary）：用 anyhow，方便传播和上下文包装
use anyhow::{Context, Result};

fn run() -> Result<()> {
    let content = std::fs::read_to_string("config.toml")
        .context("failed to read config")?;   // 附加上下文信息
    Ok(())
}

fn main() -> Result<()> {   // main 也可以返回 Result，错误打印+非零退出码
    run()?;
    Ok(())
}
```

**经验法则**：写库用 `thiserror` 定义错误枚举；写应用用 `anyhow` 简单传播。

### 11.5 panic!

```rust
fn main() {
    // panic! 立即崩溃（栈回退），用于程序 bug 而非预期错误
    // panic!("crash and burn");

    // unwrap/expect：Option/Result 的快捷方式，None/Err 时 panic
    let x: Option<i32> = None;
    // x.unwrap();                  // panic
    // x.expect("must have value"); // panic with message
}
```

**什么时候用 panic**：违反不变量、代码 bug、示例/原型代码。**可预期的失败（文件不存在、网络断开、输入非法）一律用 Result。**

---

## 12. 泛型与 Trait

### 12.1 泛型函数与泛型结构体

```rust
// 泛型函数：T 必须实现 PartialOrd 才能比较
fn largest<T: PartialOrd>(list: &[T]) -> &T {
    let mut max = &list[0];
    for item in list {
        if item > max {
            max = item;
        }
    }
    max
}

// 泛型结构体 + 泛型方法
struct Pair<T> {
    a: T,
    b: T,
}

impl<T: std::fmt::Debug> Pair<T> {
    fn show(&self) {
        println!("({:?}, {:?})", self.a, self.b);
    }
}

// 特定类型的 impl（只对该类型生效）
impl Pair<i32> {
    fn sum(&self) -> i32 { self.a + self.b }
}

fn main() {
    println!("{}", largest(&[1, 5, 3]));
    println!("{}", largest(&['a', 'z', 'm']));
    let p = Pair { a: 1, b: 2 };
    p.show();
    println!("{}", p.sum());
}
```

### 12.2 Trait（类似 Go 的 interface，但更强）

```rust
// 定义 trait：一组方法签名
trait Summary {
    fn author(&self) -> String;                 // 必须实现

    fn summarize(&self) -> String {             // 默认实现
        format!("(more from {}...)", self.author())
    }
}

struct Article {
    title: String,
    author: String,
}

impl Summary for Article {
    fn author(&self) -> String { self.author.clone() }
    // summarize 用默认实现
}

struct Tweet {
    user: String,
    text: String,
}

impl Summary for Tweet {
    fn author(&self) -> String { self.user.clone() }
    fn summarize(&self) -> String { format!("{}: {}", self.user, self.text) }
}

fn main() {
    let a = Article { title: "Rust".into(), author: "yuan".into() };
    let t = Tweet { user: "bob".into(), text: "hello".into() };
    println!("{}", a.summarize());
    println!("{}", t.summarize());
}
```

### 12.3 trait 约束（bounds）的两种写法

```rust
// 方式1：impl Trait（简洁，参数只能是一种具体类型）
fn notify(item: &impl Summary) {
    println!("breaking: {}", item.summarize());
}

// 方式2：泛型 + trait bound（等价，但可表达更复杂的约束）
fn notify2<T: Summary>(item: &T) {}
fn notify3<T: Summary + Display>(item: &T) {}       // 多重约束
fn notify4<T>(item: &T)
where
    T: Summary + Clone,                              // where 子句（约束多时更可读）
    U: Debug,
{}

// 返回 impl Trait
fn make_summary() -> impl Summary {
    Tweet { user: "bot".into(), text: "hi".into() }
}
```

### 12.4 trait 对象：动态分发（类似 Go interface 的用法）

```rust
fn main() {
    // 静态分发（泛型）：编译期单态化，零开销，但会生成多份代码
    // 动态分发（trait 对象）：运行时查虚表，一个函数处理多种类型

    let objs: Vec<Box<dyn Summary>> = vec![
        Box::new(Article { title: "t".into(), author: "a".into() }),
        Box::new(Tweet { user: "u".into(), text: "x".into() }),
    ];
    for o in &objs {
        println!("{}", o.summarize());
    }
}
```

**选择**：
- 泛型 `<T: Trait>`：性能敏感、同类型集中处理 → 静态分发。
- `dyn Trait`：需要异构集合、插件式设计 → 动态分发（需要 `Box`/`&`/`Arc` 包裹）。

### 12.5 常用标准库 trait

```rust
use std::fmt;

// Debug：{:?} 打印；一般 derive
#[derive(Debug, Clone, PartialEq)]
struct P { x: i32, y: i32 }

// Display：{} 打印；手动实现
impl fmt::Display for P {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

// 其他重要 trait：
// Clone/Copy      —— 克隆语义（见第6节）
// PartialEq/Eq    —— == 比较；Hash —— 用作 HashMap key
// PartialOrd/Ord  —— < > 排序
// Default         —— 默认值
// From/Into       —— 类型转换
// Drop            —— 析构（RAII，离开作用域自动调用）
// Iterator        —— 迭代器（见第14节）
// Send/Sync       —— 并发安全标记（见第17节）

fn main() {
    let p = P { x: 1, y: 2 };
    println!("{:?}", p);   // Debug
    println!("{}", p);     // Display
    let q = p.clone();     // Clone
    let r = P::default();  // Default（需 derive Default）
}
```

**derive 宏**：`#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]` 让编译器自动生成实现，日常大量使用。

---

## 13. 生命周期

生命周期确保**引用在使用期间始终有效**，是纯编译期概念，**运行时零开销**。

### 13.1 为什么需要标注

```rust
// 这个函数编译不过：返回值的生命周期依赖于 x 还是 y？编译器不知道
// fn longest(x: &str, y: &str) -> &str {
//     if x.len() > y.len() { x } else { y }
// }

// 生命周期标注：告诉编译器参数和返回值的生命周期关系
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {
    if x.len() > y.len() { x } else { y }
}

// 含义：返回值的存活时间 = x 和 y 中较短的那个

fn main() {
    let s1 = String::from("long string");
    let result;
    {
        let s2 = String::from("hi");
        result = longest(s1.as_str(), s2.as_str());
        println!("{result}");    // OK：s2 还活着
    }
    // println!("{result}");     // 错误！s2 已释放，result 可能悬垂
}
```

### 13.2 生命周期省略规则（大部分情况不用写）

编译器有推断规则，以下情况**无需标注**：

1. 每个引用参数分配独立生命周期；
2. 只有一个输入引用参数 → 返回值用它的生命周期；
3. 方法有 `&self`/`&mut self` → 返回值用 self 的生命周期。

```rust
// 都不需要标注：
fn first_word(s: &str) -> &str { ... }         // 规则2
impl S {
    fn get(&self) -> &String { &self.data }    // 规则3
}
```

**只有当编译器无法推断时才需要手动标注** —— 常见于：返回值生命周期与多个参数相关、结构体持有引用。

### 13.3 结构体中的生命周期

```rust
// 结构体持有引用时必须标注
struct Excerpt<'a> {
    part: &'a str,   // Excerpt 实例的存活时间不能超过 part 引用的数据
}

impl<'a> Excerpt<'a> {
    fn len(&self) -> usize { self.part.len() }
}

fn main() {
    let novel = String::from("call me ishmael. some years ago...");
    let e = Excerpt { part: novel.as_str() };   // novel 必须比 e 活得长
    println!("{}", e.len());
}
```

### 13.4 'static

```rust
// 'static：存活于整个程序运行期
let s: &'static str = "I live forever";   // 字符串字面量都是 'static
```

> 学习建议：生命周期是 Rust 最难的概念，但**实际写代码时 90% 场景编译器自动推断**，剩下 10% 从 `&self` 开始试，报错信息会告诉你该加什么标注。不要一开始就死磕理论。

---

## 14. 闭包与迭代器

### 14.1 闭包：捕获环境的匿名函数

```rust
fn main() {
    // 语法：|参数| 表达式
    let add = |a: i32, b: i32| a + b;
    let add2 = |a, b| a + b;   // 类型可推断
    println!("{}", add(1, 2));

    // 捕获外部变量（关键区别于普通函数）
    let factor = 10;
    let mul = |x| x * factor;    // 捕获 factor（按引用）
    println!("{}", mul(3));      // 30

    // 三种捕获方式（编译器自动选择最小权限）：
    // 1. Fn(&self)      不可变借用捕获 → 可多次调用
    // 2. FnMut(&mut self) 可变借用捕获   → 修改捕获的变量
    // 3. FnOnce(self)   拿走所有权      → 只能调用一次

    let mut count = 0;
    let mut incr = || count += 1;   // FnMut（修改了 count）
    incr();
    incr();
    println!("{count}");            // 2

    let name = String::from("yuan");
    let greet = move || println!("hi {name}");  // move：强制拿走所有权
    greet();
    // println!("{name}");          // 错误！name 被 move 走了
    // move 常见场景：把闭包发给另一个线程
}
```

### 14.2 迭代器：零成本抽象的典范

```rust
fn main() {
    let v = vec![1, 2, 3, 4, 5];

    // 迭代器是惰性的：不调用消费方法就不会执行
    let iter = v.iter().map(|x| x * 2);   // 此时什么都没发生

    // 消费适配器：把迭代器消耗掉
    let sum: i32 = v.iter().sum();
    let max = v.iter().max();
    let count = v.iter().count();
    let collected: Vec<i32> = v.iter().map(|x| x * 2).collect();  // collect 最常用
    let found = v.iter().find(|x| **x > 3);       // Option<&&i32>

    // 迭代适配器：返回新迭代器（链式调用）
    // .iter()        → &T
    // .iter_mut()    → &mut T
    // .into_iter()   → T（拿走所有权）

    let evens: Vec<i32> = v.iter()
        .filter(|x| *x % 2 == 0)      // 过滤
        .map(|x| x * 10)              // 变换
        .collect();

    // fold：折叠（类似 Go 手写循环累加）
    let product = v.iter().fold(1, |acc, x| acc * x);

    // zip / enumerate / chain / take / skip
    let a = vec![1, 2, 3];
    let b = vec!["x", "y", "z"];
    let pairs: Vec<(i32, &str)> = a.iter().cloned().zip(b.iter().cloned()).collect();
    // [(1,"x"), (2,"y"), (3,"z")]

    // 与 Go 对比：
    // Go:  for _, x := range v { if x%2==0 { out = append(out, x*10) } }
    // Rust: v.iter().filter(...).map(...).collect()
    // 且 Rust 迭代器编译后和手写循环一样快（零成本抽象）
}
```

**实践建议**：数据处理优先用迭代器链，比命令式循环更安全（不会越界/索引错误），性能相同。

---

## 15. 智能指针

### 15.1 Box<T>：堆分配

```rust
fn main() {
    // Box：把值放到堆上，栈上只留指针
    let b = Box::new(5);

    // 场景1：递归类型（编译期不知道大小）
    enum List {
        Cons(i32, Box<List>),
        Nil,
    }
    use List::{Cons, Nil};
    let list = Cons(1, Box::new(Cons(2, Box::new(Nil))));

    // 场景2：大对象避免栈拷贝
    // 场景3：trait 对象 Box<dyn Trait>
}
```

### 15.2 Rc<T>：引用计数（多所有者）

```rust
use std::rc::Rc;

fn main() {
    // Rc：单线程下的共享所有权（引用计数）
    let a = Rc::new(String::from("shared"));
    let b = Rc::clone(&a);        // 引用计数+1，不深拷贝
    let c = Rc::clone(&a);
    println!("count = {}", Rc::strong_count(&a));  // 3
    // Rc 是不可变的！不能通过 Rc 修改内部值

    // 循环引用会泄漏（计数永不归零），需要 Weak 打破环
}
```

### 15.3 RefCell<T>：运行时借用检查（内部可变性）

```rust
use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    // RefCell：把借用规则检查从编译期推迟到运行期
    let data = RefCell::new(5);

    let mut borrow = data.borrow_mut();   // 可变借用
    *borrow += 1;
    drop(borrow);                          // 必须先释放

    let r = data.borrow();                 // 不可变借用
    println!("{}", *r);
    // 如果同时 borrow() 和 borrow_mut() → 运行时 panic
}
```

**Rc<RefCell<T>> 组合**：单线程下"多个所有者 + 可修改"，图、树结构常用。多线程用 `Arc<Mutex<T>>`（见第 17 节）。

### 15.4 Deref 与 Drop（智能指针的机制）

```rust
use std::ops::Deref;

struct MyBox<T>(T);

impl<T> MyBox<T> {
    fn new(x: T) -> Self { MyBox(x) }
}

// 实现 Deref 后可以自动解引用（*y）和隐式转换（&MyBox<String> → &String → &str）
impl<T> Deref for MyBox<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.0 }
}

// 实现 Drop：离开作用域自动调用（RAII 模式，非常常用！）
struct Connection;
impl Drop for Connection {
    fn drop(&mut self) {
        println!("connection closed");   // 文件句柄、锁、socket 自动释放
    }
}

fn main() {
    let x = MyBox::new(String::from("hello"));
    println!("{}", *x);                  // 手动解引用
    let c = Connection;
    drop(c);                             // 也可以提前手动释放
}
```

> **RAII 是 Rust 最重要的工程模式**：资源（文件、锁、内存）的生命周期绑定到对象，`Drop` 自动清理，等价于 Go 里到处手写的 `defer f.Close()`，但编译器强制保证。

---

## 16. 模块系统与项目组织

### 16.1 模块与可见性

```rust
// src/main.rs 或 src/lib.rs

mod network {                  // 定义模块
    pub fn connect() {}        // pub：对外可见；默认私有！
    fn helper() {}             // 私有，模块外不可访问

    pub mod tcp {
        pub fn listen() {}
        pub(crate) fn internal() {}   // crate 内可见
    }
}

fn main() {
    network::connect();                 // 完整路径
    use network::tcp::listen;           // use 引入（类似 Go 的 import 别名）
    listen();
}
```

### 16.2 文件组织

```
src/
├── main.rs          # 二进制入口
├── lib.rs           # 库入口（pub use 重导出公共 API）
├── network.rs       # 模块：mod network 对应 network.rs
└── network/         # 子模块目录
    ├── mod.rs       # 或 network/mod.rs（两种风格等价）
    ├── tcp.rs
    └── udp.rs
```

```rust
// lib.rs —— 库的公共 API
mod network;               // 声明模块（从 network.rs 加载）
pub use network::tcp;      // 重导出，外部可以 crate_name::tcp::xxx

// network.rs
pub mod tcp;               // 声明子模块（从 network/tcp.rs 加载）
pub mod udp;
```

### 16.3 use 的各种用法

```rust
use std::collections::HashMap;
use std::io::{self, Read, Write};      // 嵌套引入
use std::fmt::Result as FmtResult;     // 重命名避免冲突
pub use crate::network::tcp;           // 重导出
```

### 16.4 workspace（多 crate 项目）

```
my-project/
├── Cargo.toml          # [workspace] members = ["core", "cli"]
├── core/               # 库 crate
└── cli/                # 可执行 crate，依赖 core
```

---

## 17. 并发编程

### 17.1 线程

```rust
use std::thread;
use std::time::Duration;

fn main() {
    // 创建线程
    let handle = thread::spawn(|| {
        for i in 1..10 {
            println!("spawn: {i}");
            thread::sleep(Duration::from_millis(1));
        }
        42    // 线程可以有返回值
    });

    println!("main thread");

    let result = handle.join().unwrap();   // 等待线程结束（类似 wg.Wait + 取返回值）
    println!("thread returned {result}");
}
```

### 17.2 线程间传数据：闭包需要 move

```rust
use std::thread;

fn main() {
    let data = vec![1, 2, 3];

    // 闭包引用了 data，但线程可能活得比 main 的栈帧久 → 必须 move
    let handle = thread::spawn(move || {
        println!("{:?}", data);    // data 的所有权移入线程
    });

    handle.join().unwrap();
}
```

这就是 `Send`/`Sync` 的作用：
- **Send**：类型可以安全地**转移**到另一个线程。
- **Sync**：类型可以安全地被**多个线程共享引用**（`T: Sync` ⟺ `&T: Send`）。
- 编译器自动为大多数类型实现，并在线程边界强制检查 —— **数据竞争在编译期就报错**。

### 17.3 Channel

```rust
use std::sync::mpsc;   // multi-producer, single-consumer
use std::thread;

fn main() {
    let (tx, rx) = mpsc::channel();   // 类似 Go 的 unbuffered chan

    thread::spawn(move || {
        for i in 0..5 {
            tx.send(i).unwrap();
        }
        // tx drop 时 channel 关闭，rx 迭代结束
    });

    // 接收：recv() 阻塞；try_recv() 非阻塞
    for received in rx {               // rx 可当迭代器，channel 关闭时结束
        println!("got {received}");
    }
}
```

### 17.4 共享状态：Mutex + Arc

```rust
use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    // Arc：原子引用计数（多线程版 Rc）
    // Mutex：互斥锁（Rust 的 Mutex 内含数据，不是 Go 那种独立的 sync.Mutex）
    let counter = Arc::new(Mutex::new(0));

    let mut handles = vec![];
    for _ in 0..10 {
        let counter = Arc::clone(&counter);
        let handle = thread::spawn(move || {
            let mut num = counter.lock().unwrap();   // 加锁，返回守卫（guard）
            *num += 1;
        });   // guard 离开作用域自动解锁（RAII，忘了解锁是不可能的）
        handles.push(handle);
    }
    for h in handles { h.join().unwrap(); }

    println!("result: {}", *counter.lock().unwrap());   // 10
}
```

**对比 Go**：Go 的 `sync.Mutex` 和数据是分离的，忘了锁就出 bug；Rust 的 `Mutex<T>` 把数据锁在内部，**不拿锁根本访问不到数据**，这是类型系统层面的保证。

### 17.5 RwLock 与其他同步原语

```rust
use std::sync::RwLock;

fn main() {
    let lock = RwLock::new(5);
    let r1 = lock.read().unwrap();     // 多读
    let mut w = lock.write().unwrap(); // 单写
    *w += 1;
}
```

---

## 18. 异步编程 async/await

> Rust 的 async 是**无栈协程**，语言只提供 `async/await` 语法，运行时（executor）由库提供 —— 最主流的是 **tokio**。和 Go 自带调度器不同。

### 18.1 基础概念

```rust
// async fn 返回一个 Future（惰性！不 await 就不执行，这和 Go 的 goroutine 完全不同）
async fn fetch_data(url: &str) -> String {
    format!("data from {url}")
}

// .await：等待 Future 完成（只能在 async 上下文中使用）
// async main 需要 tokio 宏
#[tokio::main]
async fn main() {
    let result = fetch_data("http://example.com").await;
    println!("{result}");
}
```

### 18.2 并发执行：join!

```rust
use tokio::time::{sleep, Duration};

async fn task(name: &str, ms: u64) -> String {
    sleep(Duration::from_millis(ms)).await;
    format!("{name} done")
}

#[tokio::main]
async fn main() {
    // 并发运行多个 Future，等全部完成
    let (a, b) = tokio::join!(
        task("A", 100),
        task("B", 200),
    );
    println!("{a}, {b}");

    // spawn：生成独立的 tokio 任务（类似 goroutine，但需要 .await JoinHandle 才等待）
    let h1 = tokio::spawn(task("C", 100));
    let h2 = tokio::spawn(task("D", 100));
    let (c, d) = (h1.await.unwrap(), h2.await.unwrap());
    println!("{c}, {d}");
}
```

### 18.3 tokio 常用设施

```rust
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    // 异步 channel
    let (tx, mut rx) = mpsc::channel(32);   // 有界 channel（背压）

    tokio::spawn(async move {
        for i in 0..5 {
            tx.send(i).await.unwrap();
        }
    });

    while let Some(msg) = rx.recv().await {
        println!("{msg}");
    }

    // 异步 Mutex：tokio::sync::Mutex（跨 .await 持锁时用）
    // 超时与取消
    let result = tokio::time::timeout(Duration::from_secs(1), sleep(Duration::from_secs(2))).await;
    // result 是 Err —— 超时

    // select!：多个 Future 竞争（谁先完成执行谁）
    tokio::select! {
        _ = sleep(Duration::from_secs(1)) => println!("timeout"),
        msg = rx.recv() => println!("got {msg:?}"),
    }
}
```

### 18.4 async 学习要点

1. **Future 是惰性的**：不 `.await` 不执行（Go 的 goroutine 立即执行，这是最大心智差异）。
2. **async 函数内部不能阻塞**：不要在 async 上下文调用阻塞 IO/CPU 密集循环 —— 用 `tokio::task::spawn_blocking` 或 `rayon`。
3. **跨 `.await` 持有引用有生命周期限制**，复杂的用 `Arc` 或 `spawn` 所有权。
4. `Send` bound：tokio 任务默认可能在多线程间迁移，闭包捕获的东西要 `Send + 'static`。

> 建议：系统编程先用 `std::thread` 写同步代码，性能不够或连接量大（万级并发连接）再上 tokio。

---

## 19. 测试与文档

### 19.1 单元测试

```rust
// src/lib.rs 中直接写（测试代码与业务代码同文件）
pub fn add(a: i32, b: i32) -> i32 { a + b }

pub fn divide(a: i32, b: i32) -> Result<i32, String> {
    if b == 0 { return Err("divide by zero".to_string()); }
    Ok(a / b)
}

#[cfg(test)]                    // 只在 cargo test 时编译
mod tests {
    use super::*;               // 引入父模块的所有项

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
        assert_ne!(add(2, 3), 6);
        assert!(add(1, 1) > 0, "custom message {}", "here");
    }

    #[test]
    fn test_divide() {
        assert_eq!(divide(10, 2), Ok(5));
        assert!(divide(1, 0).is_err());
    }

    #[test]
    #[should_panic(expected = "index out of range")]   // 期望 panic 的测试
    fn test_panic() {
        let v: Vec<i32> = vec![];
        v[0];
    }

    #[test]
    fn test_result() -> Result<(), String> {    // 测试可以返回 Result
        let r = divide(10, 2)?;
        assert_eq!(r, 5);
        Ok(())
    }

    #[test]
    #[ignore]                    // 默认跳过，cargo test -- --ignored 运行
    fn expensive_test() {}
}
```

```bash
cargo test                    # 运行所有测试
cargo test test_add           # 按名字过滤
cargo test -- --nocapture     # 显示 println 输出
cargo test -- --test-threads=1  # 单线程跑
```

### 19.2 集成测试

```
mylib/
├── src/lib.rs
└── tests/            # 每个文件是独立的集成测试 crate
    └── api.rs        # 只能访问库的公共 API
```

```rust
// tests/api.rs
use mylib::add;    // 像外部用户一样使用

#[test]
fn test_public_api() {
    assert_eq!(add(1, 2), 3);
}
```

### 19.3 文档测试（Rust 特色：文档里的代码自动变成测试）

```rust
/// 两数相加。
///
/// # Examples
///
/// ```
/// let result = mylib::add(2, 3);
/// assert_eq!(result, 5);
/// ```
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

```bash
cargo test         # 文档测试自动运行
cargo doc --open   # 生成 HTML 文档（/// 注释会渲染进去）
```

### 19.4 benchmark

```rust
// 简单方式：std::hint::black_box 防止优化掉
#[bench] 已移除稳定版；推荐 criterion：
// Cargo.toml: [dev-dependencies] criterion = "0.5"
// benches/my_bench.rs 用 criterion::black_box + group.bench_function
// 或者简单点：cargo build --release 后用 hyperfine 命令行测量
```

---

## 20. 宏（入门）

宏在**编译期做代码生成**，感叹号是标志。分声明宏和过程宏两类。

### 20.1 声明宏（macro_rules!）

```rust
// vec!、println!、assert! 都是声明宏
macro_rules! my_vec {
    // 模式 => 展开代码；$(...),* 表示重复
    ( $( $x:expr ),* ) => {
        {
            let mut v = Vec::new();
            $( v.push($x); )*
            v
        }
    };
}

fn main() {
    let v = my_vec![1, 2, 3];
    println!("{:?}", v);
}
```

### 20.2 过程宏（了解即可）

```rust
// derive 宏：为结构体自动实现 trait（你已经用过很多次了）
#[derive(Debug, Clone, Serialize)]   // 这些都是过程宏生成的代码

// 属性宏 / 函数式宏：#[tokio::main]、sqlx::query! 等
```

> 宏学习优先级低：先用别人的宏（derive、tokio::main、thiserror），需要自定义时再查资料。

---

## 21. Unsafe Rust（入门）

`unsafe` 块关闭部分编译器检查，允许：裸指针解引用、调用 unsafe 函数/Foli、访问/修改可变静态变量、实现 unsafe trait。

```rust
fn main() {
    let mut num = 5;

    // 裸指针：创建不需要 unsafe，解引用才需要
    let r1 = &num as *const i32;
    let r2 = &mut num as *mut i32;

    unsafe {
        println!("{}", *r1);   // 绕过借用检查（你自己负责正确性）
        *r2 = 10;
    }
}
```

**什么时候需要**：FFI（调用 C 库）、极致性能的手写数据结构（如自己实现 buffer 管理）、与操作系统/硬件交互。

> 你的 URMA/RDMA 场景大概率会碰到 unsafe（mmap、注册内存区域、C 绑定）。原则：**unsafe 块越小越好，把 unsafe 封装在安全 API 内部**，并写清安全前提（invariant）注释。

---

## 22. 实战项目建议

按难度递进，建议结合你现有的领域（网络传输/性能基准测试）：

### 阶段一：语法熟练（CLI 工具）

1. **命令行 TODO / grep 工具** —— 练：所有权、String/&str、Result、错误处理、文件 IO
   - 参数解析用 `clap` crate
2. **日志文件分析器** —— 练：迭代器链、HashMap 统计、性能测量

### 阶段二：数据结构与并发

3. **并发端口扫描器** —— 练：thread、channel、Arc<Mutex>
4. **TCP echo server（同步版）** —— 练：std::net、线程池
5. **KV 存储（内存版）** —— 练：trait 设计、Rc/RefCell vs Arc<Mutex> 的选型

### 阶段三：网络与异步

6. **HTTP 服务器（axum 或手写）** —— 练：async/await、tokio
7. **带宽/延迟基准测试工具**（对你最有价值）—— 练：socket 编程、性能计数、criterion
8. **简易 RPC/消息传输框架** —— 练：trait 对象做抽象、serde 序列化、tokio 网络

### 常用 crate 速查（生态是 Rust 的强项）

| 用途 | crate |
|------|-------|
| CLI 参数 | clap |
| 错误处理 | thiserror（库）/ anyhow（应用） |
| 序列化 | serde + serde_json |
| 异步运行时 | tokio |
| HTTP 服务 | axum / hyper |
| HTTP 客户端 | reqwest |
| 日志 | tracing（推荐）/ log |
| 正则 | regex |
| 随机 | rand |
| 数据库 | sqlx |
| 基准测试 | criterion |

---

## 23. 学习路线与资源

### 23.1 建议路线（4~8 周）

```
第 1 周   环境搭建 + 基础语法（第 3~5 节） + Cargo 熟练使用
第 2 周   所有权/借用/切片（第 6~7 节）—— 最关键的一周，多写多报错多读错误信息
第 3 周   结构体/枚举/match/集合/错误处理（第 8~11 节），做一个 CLI 小工具
第 4 周   泛型/Trait/生命周期（第 12~13 节）+ 闭包迭代器（第 14 节）
第 5 周   智能指针（第 15 节）+ 模块系统（第 16 节），做一个中型项目
第 6 周   并发（第 17 节），写多线程程序
第 7~8 周 async/await + tokio（第 18 节），写网络程序
之后      按需：宏、unsafe、FFI
```

### 23.2 核心资源

- **The Rust Programming Language**（"the book"）：官方教程，最权威
  - 中文版：https://kaisery.github.io/trpl-zh-cn/
- **Rust by Example**：边看例子边学 https://doc.rust-lang.org/rust-by-example/
- **Rustlings**：官方练习题（强烈推荐配套做）`cargo install rustlings`
- **Rust 语言圣经**（中文，很全面）：https://course.rs/
- **std 文档**：https://doc.rust-lang.org/std/ （遇到 API 就查这个）
- **Rust 语言之旅**（交互式）：https://tourofrust.com/

### 23.3 遇到问题怎么查

1. 编译器错误信息（E0xxx）→ 官网搜错误码，解释极详细
2. `cargo clippy` 会教你更地道的写法
3. std 文档 + crate 文档（docs.rs）
4. 设计问题 → Rust API Guidelines / std 源码（标准库代码质量极高，值得读）

---

## 24. 常见编译错误与心法

### 24.1 高频错误速查

| 错误 | 原因 | 解法 |
|------|------|------|
| `cannot move out of...` / `value moved` | 所有权被移走了 | 用引用 `&`，或 `clone()` |
| `borrowed value does not live long enough` | 引用比数据活得久 | 调整作用域/所有权，或加生命周期 |
| `cannot borrow as mutable` | 已有不可变借用 | 缩小借用范围（NLL），重构代码 |
| `expected X, found ()` | 函数最后加了分号 | 去掉分号（表达式作返回值） |
| `non-exhaustive patterns` | match 漏了分支 | 加 `_` 或补全变体 |
| `trait bound not satisfied` | 泛型缺少约束 | 加 bound 或实现 trait |
| `use of moved value` in closure | 闭包捕获了被移走的值 | `move` 或用引用捕获 |
| 生命周期标注一堆报错 | 结构复杂 | 优先考虑用所有权替代引用（返回 owned 值） |

### 24.2 心法（过来人经验）

1. **先学会"跟编译器走"**：错误信息是 Rust 最好的老师，从上到下逐条修复。
2. **借用检查卡住时，先想"能不能直接 move/clone"**：初学阶段 clone 是完全正当的手段，先跑通再优化。
3. **设计上"拥有数据"比"到处借引用"简单得多**：结构体存 `String` 而不是 `&str`，能省掉大量生命周期问题。
4. **`&self` 方法 > `&mut self` > `self`**：按需选择最小权限。
5. **不要过早用 unsafe / 宏 / 复杂生命周期**：90% 的场景安全 Rust + clone 就够了。
6. **多用 cargo check + cargo clippy**：比反复编译运行快得多。
7. **引用是一等公民但要节制**：需要共享可变状态时，`Arc<Mutex<T>>` 是标准答案。
8. **前两周怀疑人生是正常的**：所有权模型一旦"开窍"（通常在第 2~3 周），之后就是一门非常高效的语言。

### 24.3 Go → Rust 习惯迁移清单

| Go 习惯 | Rust 对应 |
|---------|-----------|
| `b := a`（slice/map 拷贝头） | `let b = a;` 是 **move**，想拷贝要 `.clone()` |
| `nil` 检查 | `Option<T>` + `match`/`if let` |
| `if err != nil { return err }` | `let x = f()?;` |
| `go func()` | `thread::spawn` / `tokio::spawn` |
| `chan T` | `mpsc::channel` / `tokio::sync::mpsc` |
| `sync.Mutex`（数据外置） | `Mutex<T>`（数据内置，锁 guard 解引用） |
| `defer f.Close()` | RAII + `Drop`（自动，写不了错） |
| `make([]int, 10)` | `vec![0; 10]` |
| `range` | `iter()` / `iter_mut()` / `into_iter()` |
| `interface{}` / `any` | `Box<dyn Any>` / `enum` / 泛型 |
| 指针接收者 | `&mut self` |
| 大写导出 | `pub` 关键字 |
