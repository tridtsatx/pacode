//! Golden extraction tests: real snippets per language, asserting symbol
//! names, kinds, nesting, and parse-error bookkeeping.
use pacode_symbols::{Language, SymbolKind, extract};

/// Render `(kind, indent, name, start-end)` lines for compact assertions.
fn render(lang: Language, src: &str) -> String {
    let file = extract(src, lang, Default::default()).unwrap();
    let mut out = String::new();
    if file.parsed_with_errors {
        out.push_str(&format!("ERR@{:?}\n", file.first_error_line));
    }
    for s in &file.symbols {
        let indent = "  ".repeat(s.depth(&file.symbols));
        out.push_str(&format!(
            "{indent}{} {} {}-{}\n",
            s.kind.name(),
            s.name,
            s.start_line,
            s.end_line
        ));
    }
    out
}

fn kinds(lang: Language, src: &str) -> Vec<(String, SymbolKind)> {
    let file = extract(src, lang, Default::default()).unwrap();
    file.symbols
        .iter()
        .map(|s| (s.name.clone(), s.kind))
        .collect()
}

#[test]
fn rust_nested() {
    let src = "\
const LIMIT: u32 = 10;
static FLAG: bool = false;

struct Config {
    a: u8,
}

enum Mode { On, Off }

trait Run {
    fn run(&self);
}

impl Config {
    fn new() -> Self {
        Self { a: 0 }
    }
}

impl Run for Config {
    fn run(&self) {}
}

mod inner {
    fn helper() {}
}

macro_rules! m {
    () => {};
}

fn top() {
    let closure = |x| x;
}
";
    insta::assert_snapshot!(render(Language::Rust, src));
}

#[test]
fn python_nested() {
    let src = "\
TOP = 1

def outer(a):
    def inner(b):
        return b
    return inner(a)

@dec
def decorated():
    pass

class Foo:
    X = 2

    def __init__(self):
        self.x = 1

    @property
    def prop(self):
        return self.x

@dataclass
class Bar:
    y: int = 0
";
    insta::assert_snapshot!(render(Language::Python, src));
}

#[test]
fn typescript_nested() {
    let src = "\
export interface Shape {
    area(): number;
    readonly name: string;
}

export abstract class Base {
    private x = 1;
    constructor() {}
    abstract draw(): void;
    helper() { return 1; }
}

const top = 5;
export const ratio = 2;
export const calc = (n: number) => n * 2;

enum Color { Red, Blue }

type Alias = Shape | Base;

namespace NS {
    export function inner() {}
}

export function free(): void {}

declare module 'fs' {
    function read(): void;
}
";
    insta::assert_snapshot!(render(Language::TypeScript, src));
}

#[test]
fn javascript_nested() {
    let src = "\
const C = 1;
let helper = () => {};
var legacy = function named() {};

function decl() {}

class Klass {
    #priv = () => {};
    constructor() {}
    method() { return 1; }
    get value() { return 2; }
}

export default function main() {}
export { decl, Klass };
export const e = () => {};
";
    insta::assert_snapshot!(render(Language::JavaScript, src));
}

#[test]
fn go_nested() {
    let src = "\
package main

const Version = \"1.0\"
var counter int

type Server struct {
    addr string
}

type Handler interface {
    Handle()
}

type Alias = Server

func NewServer() *Server {
    return &Server{}
}

func (s *Server) Start() error {
    f := func() {}
    _ = f
    return nil
}

func (s Server) Stop() {}

type (
    A struct{}
    B = int
)
";
    insta::assert_snapshot!(render(Language::Go, src));
}

#[test]
fn c_nested() {
    let src = "\
#define MAX 100
#define SQR(x) ((x) * (x))

struct Point {
    int x;
};

union Data { int i; float f; };

enum Color { RED, GREEN };

typedef struct Point Point;
typedef unsigned long size_t2;

int *dup(const char *s);
static void proto(int a);

int add(int a, int b) {
    return a + b;
}

static int helper(void) {
    return 0;
}
";
    insta::assert_snapshot!(render(Language::C, src));
}

#[test]
fn cpp_nested() {
    let src = "\
namespace ns {

class Widget : public Base {
public:
    Widget();
    ~Widget();
    void draw() {}
    int get() const;
private:
    int x_;
};

struct S { int v; };

enum class Color { R, G };

template <typename T>
T identity(T v) { return v; }

} // namespace ns

int free_fn() { return 0; }

int ns::Widget::get() const { return 1; }

using Alias = ns::Widget;
typedef int myint;
";
    insta::assert_snapshot!(render(Language::Cpp, src));
}

#[test]
fn java_nested() {
    let src = "\
package com.example;

@interface Tag {
    String value();
}

enum Level { LOW, HIGH }

interface Repo {
    int LIMIT = 5;
    void save();
    default void flush() {}
}

record Point(int x, int y) {}

class Outer {
    static class Inner {
        void m() {}
    }
    Outer() {}
    int count() { return 0; }
}
";
    insta::assert_snapshot!(render(Language::Java, src));
}

#[test]
fn csharp_nested() {
    let src = "\
using System;

namespace App.Core {
    const int Top = 1;

    public class Widget : Base {
        public const int Max = 5;
        public string Name { get; set; }
        public Widget() {}
        ~Widget() {}
        public void Draw() {}
        private int Calc() { return 1; }
        public static Widget operator +(Widget a, Widget b) => a;
    }

    public struct S { public int V; }
    public interface IRepo { void Save(); }
    public enum Color { Red }
    public delegate void Callback(int x);
    public record Rec(int A);
}

file static class F { }
";
    insta::assert_snapshot!(render(Language::CSharp, src));
}

#[test]
fn ruby_nested() {
    let src = "\
module Outer
  class Foo < Base
    CONST = 1

    def initialize(x)
      @x = x
    end

    def self.build
      new(0)
    end

    def run
      helper
    end

    private

    def helper
      1
    end
  end

  def self.top_level
  end
end
";
    insta::assert_snapshot!(render(Language::Ruby, src));
}

#[test]
fn php_nested() {
    let src = "\
<?php
namespace App\\Core;

const LIMIT = 10;

interface Repo {
    public function save();
}

trait Log {
    public function log() {}
}

enum Status {
    case Ok;
}

class Service extends Base implements Repo {
    private int $x = 0;
    public function __construct() {}
    public function save() {}
}

function free_helper() {}
";
    insta::assert_snapshot!(render(Language::Php, src));
}

#[test]
fn swift_nested() {
    let src = "\
protocol Drawable {
    func draw()
}

struct Point {
    var x: Int
    let origin = 0
    func move() {}
}

enum Dir {
    case north, south
}

class View: Drawable {
    init() {}
    deinit {}
    func draw() {}
    func layout() -> Int { return 1 }
}

extension View {
    func extra() {}
}

func free(_ a: Int) -> Int { a }
";
    insta::assert_snapshot!(render(Language::Swift, src));
}

#[test]
fn kotlin_nested() {
    let src = "\
package com.x

interface Repo {
    fun save(): Unit
}

enum class Kind { A, B }

data class User(val id: Int) {
    fun label() = \"x\"
}

object Registry {
    fun get() = 1
}

class Outer {
    inner class Inner { fun m() {} }
    constructor(a: Int) {}
}

fun Int.double() = this * 2
fun free() {}
val top = 1
";
    insta::assert_snapshot!(render(Language::Kotlin, src));
}

#[test]
fn scala_nested() {
    let src = "\
package com.x

object App {
  val Version = \"1\"
  def main(): Unit = {
    def local() = 1
    local()
  }
}

trait Repo {
  def save(): Int
}

class Service extends Repo {
  def save(): Int = 1
  def helper(): Int = 2
}

enum Color { case Red, Blue }

case class User(id: Int)

extension (s: String)
  def shout: String = s

type Alias = Int
";
    insta::assert_snapshot!(render(Language::Scala, src));
}

#[test]
fn lua_nested() {
    let src = "\
local function helper() end

function M.f() end

function M.sub:method() end

M = {
  run = function() end,
}

local closure = function() end

M.extra = function() end
";
    insta::assert_snapshot!(render(Language::Lua, src));
}

#[test]
fn bash_nested() {
    let src = "\
VERSION=1

deploy() {
  echo hi
}

function cleanup {
  rm -f x
}

main() {
  inner() { echo nested; }
  inner
}
";
    insta::assert_snapshot!(render(Language::Bash, src));
}

#[test]
fn parse_error_flagged() {
    let src = "fn ok() {}\nfn broken( {\n  let x = ;\n";
    let file = extract(src, Language::Rust, Default::default()).unwrap();
    assert!(file.parsed_with_errors);
    assert_eq!(file.first_error_line, Some(2));
    assert!(file.symbols.iter().any(|s| s.name == "ok"));
}

#[test]
fn parse_error_still_lists_symbols() {
    // Even with errors, valid symbols are extracted.
    let src = "def good():\n    return 1\n\ndef bad(:\n";
    let file = extract(src, Language::Python, Default::default()).unwrap();
    assert!(file.parsed_with_errors);
    assert!(file.symbols.iter().any(|s| s.name == "good"));
}

#[test]
fn oversize_rejected() {
    let src = "x".repeat(2 * 1024 * 1024 + 1);
    let err = extract(&src, Language::Rust, Default::default()).unwrap_err();
    assert!(err.to_string().contains("too large"));
}

#[test]
fn qualified_rust_names() {
    let src = "impl Foo {\n    fn method(&self) {}\n}\nimpl Drop for Foo {\n    fn drop(&mut self) {}\n}\n";
    let syms = kinds(Language::Rust, src);
    assert_eq!(
        syms,
        vec![
            ("Foo".to_string(), SymbolKind::Impl),
            ("method".to_string(), SymbolKind::Method),
            ("Drop for Foo".to_string(), SymbolKind::Impl),
            ("drop".to_string(), SymbolKind::Method),
        ]
    );
}

#[test]
fn go_method_receiver_name() {
    let src = "package x\n\nfunc (s *Server) Start() {}\nfunc (s Server) Stop() {}\n";
    let syms = kinds(Language::Go, src);
    assert_eq!(
        syms,
        vec![
            ("Server.Start".to_string(), SymbolKind::Method),
            ("Server.Stop".to_string(), SymbolKind::Method),
        ]
    );
}

#[test]
fn empty_source() {
    let file = extract("", Language::Rust, Default::default()).unwrap();
    assert!(!file.parsed_with_errors);
    assert!(file.symbols.is_empty());
}

#[test]
fn line_numbers_are_one_based_inclusive() {
    let src = "// comment\nfn a() {}\n\nfn b() {\n}\n";
    let file = extract(src, Language::Rust, Default::default()).unwrap();
    let a = &file.symbols[0];
    let b = &file.symbols[1];
    assert_eq!((a.name.as_str(), a.start_line, a.end_line), ("a", 2, 2));
    assert_eq!((b.name.as_str(), b.start_line, b.end_line), ("b", 4, 5));
}
