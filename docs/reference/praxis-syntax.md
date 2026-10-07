# Praxis（`.px`）语法参考

本文描述当前编译器已经实现的 Praxis 语法。源码文件扩展名是 `.px`；完整程序必须定义一个
无参数的 `main()`，Shell 中输入的代码则按交互模式编译。

## 最小程序

```px
func main() {
    terminal = object.find("terminal")
    terminal.println("Hello, Ousject")
}
```

运行：

```bash
./target/release/ousject run hello.px --memory --local
```

完整程序顶层只能放 `func`、`class`、`public class` 和 `private class` 声明，并且必须恰好
定义一个 `func main()`。`main()` 不能带参数。Shell 交互模式不需要 `main()`，裸表达式会
自动输出结果。

## 词法规则

- 标识符由 ASCII 字母、数字、下划线和点组成，不能以数字开头。点用于字段访问或能力调用。
- 空格、Tab 和回车是空白；换行与 `;` 都可结束语句。
- `//` 开始单行注释。
- 字符串使用双引号，支持 `\e`（ESC）、`\n`、`\r`、`\t`、`\"` 和 `\\` 转义，并支持 UTF-8 文本。
- 整数是有符号 64 位整数；带小数点的数字是有限 `Float`。
- 关键字：`true false null if else while break continue func return class extends public`
  `private try catch transaction link new and or not`。其中 `new` 已移除，只用于给出迁移错误。

## 值与集合

```px
integer = 42
float = 3.14
text = "你好"
enabled = true
missing = null
items = [1, 2, 3]
user = { name: "Ada", "age": 18 }
```

数组用整数下标，Map/Record 用文本键，Text 用整数下标读取字符：

```px
first = items[0]
age = user["age"]
character = text[0]

items[0] = 10
user["age"] = 19
items[0]++
user["age"]--
```

`#value` 返回 Array、Map、Record、Text 或 Bytes 的长度。

## 变量与运算符

变量第一次赋值即创建，不使用 `let` 或 `var`：

```px
answer = 40 + 2
answer++
answer--
```

运算符从高到低为：

1. 一元：`!`、`not`、负号 `-`、长度 `#`
2. 乘除：`*`、`/`、`%`
3. 加减：`+`、`-`
4. 比较：`==`、`!=`、`<`、`<=`、`>`、`>=`
5. 逻辑与：`&&`、`and`
6. 逻辑或：`||`、`or`

整数运算会检查溢出。`+` 也可连接两个 Text；Integer 与 Float 混合运算返回 Float。逻辑与、
逻辑或采用短路求值。

## 条件与循环

```px
if score >= 90 {
    grade = "A"
} else if score >= 60 {
    grade = "pass"
} else {
    grade = "fail"
}

count = 0
while count < 10 {
    count++
    if count == 3 { continue }
    if count == 8 { break }
}
```

当前没有 `for`、`switch` 或三元表达式。

## 函数

```px
func add(left, right) {
    return left + right
}

func no_result() {
    return
}
```

参数没有静态类型声明。没有显式 `return` 时返回 `null`。同一函数不能声明重名参数。

## Class

```px
class Counter {
    value = 0
    private secret = 7

    func add(amount) {
        this.value = this.value + amount
        return this.value
    }
}

class FastCounter extends Counter {
    public func add(amount) {
        return super.add(amount * 2)
    }
}

counter = object.create("FastCounter", { value: 10 })
result = counter.add(3)
```

- 字段默认值必须是常量；实例初始 Record 可以覆盖公共默认字段。
- 方法内用 `this` 访问当前实例，用 `super.method(...)` 调用父类方法。
- `private` 字段和方法只能从相应类的方法内部访问。
- 当前使用 `object.create(class_name, initial_record)` 创建实例；`new` 和 `init` 构造方法已移除。
- 编译器接受顶层 `public class`/`private class`，但当前 Class 的顶层可见性尚未形成独立运行时边界。

## Object 身份、字段与 Link

变量绑定到持久 Object。点号可以读取 Object 字段或属性，也可以调用能力：

```px
item = object.create("core.text", "one")
same = object.find(item.id)
kind = item.type
item.replace("two")

alias link item
```

`alias link item` 为同一个 Object 建立另一个变量绑定，不复制值。

## 异常处理

```px
try {
    value = 1 / 0
} catch (error) {
    terminal.println(error)
}
```

`error` 是 Error 值，输出格式为 `code: message`。可捕获的运行时错误包括未定义变量、类型
错误、除零、越界、缺少键、缺少 Provider，以及多数对象访问错误。格式损坏、Worker Lease
等运行时完整性错误不可由 Praxis 捕获。

## Transaction

```px
transaction {
    item.value = 2
    values["count"]++
    alias link item
}
```

Transaction 块一次提交其中的赋值、字段/索引更新和 `link`。当前限制：

- 不允许嵌套 Transaction。
- 块中只能出现赋值、字段/索引更新或 `link`。
- 不允许函数调用和 Object 能力调用。

块内出错时本次事务不提交。

## Import 与 Include

```px
import "math"
include "generated-values"
```

指令必须独占顶层行，可以带尾部分号或 `//` 注释。`import` 在一次编译中只展开一次，
`include` 每次出现都会展开；循环依赖会报错。它们需要 CLI 或带 loader 的编译入口，普通
`compile()` 无法自行加载模块。被加载模块不能定义 `main()`。

## Shell 交互模式

Shell 会保存变量和执行位置。下面会直接输出 `3`：

```px
a = 1
a++
a + 1
```

`exit` 只退出当前 Shell 前端，保留其 Terminal Object 和交互 Process；`close-terminal` 才关闭
Shell Terminal。提交的代码发生可捕获错误时，Shell 输出错误并继续读取下一条命令。

## 当前没有的语法

当前没有静态类型标注、变量声明关键字、`for`、`switch`、闭包、匿名函数、可选链、三元
表达式、模块命名空间语法和 `new`。不要把设计草案或其他语言的写法当成现有 Praxis 语法。

运行时可调用能力见 [Praxis API 总表](praxis-api.md)。可执行例子位于仓库的
[`examples/`](../../examples)。
