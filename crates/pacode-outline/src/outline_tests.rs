//! Outline fixtures per language. Expected ranges are 1-based inclusive line numbers.
//! Each fixture is small, so the default `min_hidden` (3) decides which gaps hide.

use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;

type Shape = (Vec<(usize, usize)>, Vec<(usize, usize, Option<String>)>);

/// Kept and hidden ranges as 1-based inclusive line numbers.
fn shape(o: &Outline) -> Shape {
    let kept = o.kept.iter().map(|r| (r.start + 1, r.end)).collect();
    let hidden = o
        .hidden
        .iter()
        .map(|h| (h.lines.start + 1, h.lines.end, h.label.clone()))
        .collect();
    (kept, hidden)
}

/// A hidden region labelled with its declaration.
fn hid(a: usize, b: usize, label: &str) -> (usize, usize, Option<String>) {
    (a, b, Some(label.to_string()))
}

/// A hidden region of top-level code (no label).
fn top(a: usize, b: usize) -> (usize, usize, Option<String>) {
    (a, b, None)
}

fn run(source: &str, lang: Lang) -> Outline {
    let o = outline(source, lang, &OutlineOpts::default()).expect("outline");
    assert_covers(&o, source);
    o
}

/// `kept` and `hidden` are sorted, disjoint, non-empty and cover every line exactly once.
fn assert_covers(o: &Outline, source: &str) {
    assert_eq!(o.total_lines, source.lines().count());
    let mut next = 0;
    for segment in o.segments() {
        let range = match segment {
            Segment::Shown(r) => r.clone(),
            Segment::Hidden(h) => h.lines.clone(),
        };
        assert_eq!(range.start, next, "gap or overlap before {range:?}");
        assert!(range.end > range.start, "empty segment {range:?}");
        next = range.end;
    }
    assert_eq!(next, o.total_lines);
    for h in &o.hidden {
        assert!(
            h.lines.len() >= OutlineOpts::default().min_hidden,
            "short hidden {h:?}"
        );
    }
}

#[cfg(feature = "rust")]
const RUST: &str = r#"use std::collections::HashMap;
use std::fmt;

/// A thing.
/// More docs.
/// Even more.
/// And more.
/// Last.
#[derive(Debug)]
pub struct Foo {
    a: u32,
    b: String,
}

impl Foo {
    pub fn new(a: u32) -> Self {
        let b = a.to_string();
        let c = 1;
        let d = 2;
        let e = 3;
        let f = 4;
        Self { a, b }
    }

    fn short(&self) -> u32 {
        self.a
    }
}

const TABLE: [u32; 6] = [
    1,
    2,
    3,
    4,
    5,
];

fn main() {
    let x = 1;
    let y = 2;
    let z = 3;
    println!("{x}{y}{z}");
    println!("done");
}
"#;

#[cfg(feature = "typescript")]
const TS: &str = r#"import { a } from "a";
import { b } from "b";
import { c } from "c";
import { d } from "d";
import { e } from "e";
import { f } from "f";

export class Service {
  private cache = new Map<string, number>();

  constructor(private readonly name: string) {}

  async load(id: string): Promise<number> {
    const hit = this.cache.get(id);
    if (hit !== undefined) {
      return hit;
    }
    const value = await fetchIt(id);
    this.cache.set(id, value);
    return value;
  }
}

export const handler = async (req: Request) => {
  const body = await req.json();
  const id = body.id;
  const name = body.name;
  const age = body.age;
  return { id, name, age };
};
"#;

#[cfg(feature = "python")]
const PY: &str = r#"import os
import sys


class Repo:
    """A repository."""

    def __init__(self, root):
        self.root = root
        self.cache = {}
        self.dirty = False
        self.count = 0
        self.items = []

    @property
    def name(self):
        return os.path.basename(self.root)


def main(argv):
    repo = Repo(argv[1])
    print(repo.name)
    print(repo.root)
    print(repo.cache)
    print(repo.dirty)
    return 0
"#;

#[cfg(feature = "go")]
const GO: &str = r#"package main

import (
	"fmt"
	"os"
	"strings"
	"time"
)

type Server struct {
	Name string
	Port int
}

func (s *Server) Run(args []string) error {
	fmt.Println(s.Name)
	fmt.Println(s.Port)
	fmt.Println(len(args))
	fmt.Println(strings.Join(args, " "))
	fmt.Println(time.Now())
	return nil
}

func main() {
	os.Exit(0)
}
"#;

#[cfg(feature = "java")]
const JAVA: &str = r#"package demo;

public class Codec {
  private int size = 0;

  public void foo(Boo boo, byte[] bytes) {
    int n = bytes.length;
    if (n == 0) return;
    size += n;
    size += 1;
    size += 2;
    size += 3;
  }

  interface Sink {
    void put(byte b);
  }
}
"#;

#[cfg(feature = "json")]
const JSON: &str = r#"{
  "name": "demo",
  "scripts": {
    "build": "tsc",
    "test": "vitest"
  },
  "deps": {
    "nested": {
      "inner": {
        "a": 1,
        "b": 2,
        "c": 3,
        "d": 4
      }
    }
  }
}
"#;

#[cfg(feature = "markdown")]
const MD: &str = r#"# Title

Intro line one.
Intro line two.
Intro line three.
Intro line four.
Intro line five.

## Install

Run this.
Then that.
And this.
And more.
And more.
"#;

#[cfg(feature = "rust")]
#[test]
fn rust_fixture() {
    let (kept, hidden) = shape(&run(RUST, Lang::Rust));
    assert_eq!(kept, [(1, 4), (9, 19), (23, 32), (36, 44)]);
    assert_eq!(
        hidden,
        [
            hid(5, 8, "struct Foo"),
            hid(20, 22, "fn new"),
            hid(33, 35, "const TABLE"),
        ]
    );
}

#[cfg(feature = "typescript")]
#[test]
fn typescript_fixture() {
    let (kept, hidden) = shape(&run(TS, Lang::TypeScript));
    assert_eq!(kept, [(8, 16), (21, 30)]);
    assert_eq!(hidden, [hid(1, 7, "imports"), hid(17, 20, "method load")]);
}

#[cfg(feature = "python")]
#[test]
fn python_fixture() {
    let (kept, hidden) = shape(&run(PY, Lang::Python));
    assert_eq!(kept, [(1, 11), (15, 23)]);
    assert_eq!(
        hidden,
        [hid(12, 14, "def __init__"), hid(24, 26, "def main")]
    );
}

#[cfg(feature = "go")]
#[test]
fn go_fixture() {
    let (kept, hidden) = shape(&run(GO, Lang::Go));
    assert_eq!(kept, [(1, 1), (10, 18), (22, 26)]);
    assert_eq!(hidden, [hid(2, 9, "imports"), hid(19, 21, "func Run")]);
}

#[cfg(feature = "java")]
#[test]
fn java_fixture() {
    let (kept, hidden) = shape(&run(JAVA, Lang::Java));
    assert_eq!(kept, [(1, 9), (13, 18)]);
    assert_eq!(hidden, [hid(10, 12, "method foo")]);
}

#[cfg(feature = "json")]
#[test]
fn json_fixture() {
    let (kept, hidden) = shape(&run(JSON, Lang::Json));
    assert_eq!(kept, [(1, 9), (14, 17)]);
    assert_eq!(hidden, [hid(10, 13, "key inner")]);
}

#[cfg(feature = "markdown")]
#[test]
fn markdown_fixture() {
    let (kept, hidden) = shape(&run(MD, Lang::Markdown));
    assert_eq!(kept, [(1, 4), (9, 12)]);
    assert_eq!(
        hidden,
        [hid(5, 8, "section Title"), hid(13, 15, "section Install")]
    );
}

#[cfg(feature = "rust")]
#[test]
fn rust_symbols_nest_and_start_at_their_doc_comment() {
    fn flat(symbols: &[Symbol], out: &mut Vec<(&'static str, String, usize, usize)>) {
        for s in symbols {
            out.push((s.kind, s.name.clone(), s.lines.start + 1, s.lines.end));
            flat(&s.children, out);
        }
    }
    let mut got = Vec::new();
    flat(&run(RUST, Lang::Rust).symbols, &mut got);
    let want: [(&str, &str, usize, usize); 6] = [
        ("struct", "Foo", 4, 13),
        ("impl", "Foo", 15, 28),
        ("fn", "new", 16, 23),
        ("fn", "short", 25, 27),
        ("const", "TABLE", 30, 36),
        ("fn", "main", 38, 44),
    ];
    let want: Vec<_> = want
        .iter()
        .map(|(k, n, a, b)| (*k, n.to_string(), *a, *b))
        .collect();
    assert_eq!(got, want);
}

#[cfg(feature = "rust")]
#[test]
fn crlf_file_outlines_like_lf() {
    let crlf = RUST.replace('\n', "\r\n");
    assert_eq!(
        shape(&run(&crlf, Lang::Rust)),
        shape(&run(RUST, Lang::Rust))
    );
}

#[test]
fn empty_file_has_no_segments() {
    for lang in Lang::ALL.into_iter().filter(|l| l.is_available()) {
        assert_eq!(run("", lang), Outline::default(), "{lang:?}");
    }
}

#[cfg(feature = "rust")]
#[test]
fn body_lines_option_sets_the_kept_body() {
    let opts = OutlineOpts {
        body_lines: 1,
        ..OutlineOpts::default()
    };
    let o = outline(RUST, Lang::Rust, &opts).expect("outline");
    let (kept, hidden) = shape(&o);
    assert_eq!(kept, [(1, 4), (9, 17), (23, 30), (36, 39), (44, 44)]);
    assert_eq!(
        hidden,
        [
            hid(5, 8, "struct Foo"),
            hid(18, 22, "fn new"),
            hid(31, 35, "const TABLE"),
            hid(40, 43, "fn main"),
        ]
    );
}

#[cfg(feature = "rust")]
#[test]
fn small_syntax_error_still_outlines() {
    let broken = RUST.replace("        let c = 1;\n", "        let c = = ;\n");
    assert_eq!(
        shape(&run(&broken, Lang::Rust)),
        shape(&run(RUST, Lang::Rust))
    );
}

#[cfg(feature = "rust")]
#[test]
fn mostly_errors_is_unsupported() {
    let garbage = "fn ( { [ @@ ## $$ %% ^^ && ** \n".repeat(20);
    let err = outline(&garbage, Lang::Rust, &OutlineOpts::default()).expect_err("garbage");
    let percent = match err {
        OutlineError::Unsupported {
            reason: Unsupported::SyntaxErrors { percent },
            ..
        } => percent,
        other => panic!("unexpected {other:?}"),
    };
    assert!(percent > 30, "{percent}");
}

#[test]
fn input_over_the_cap_is_refused() {
    let opts = OutlineOpts {
        max_input_bytes: 8,
        ..OutlineOpts::default()
    };
    assert_eq!(
        outline("fn main() {}\n", Lang::Rust, &opts),
        Err(OutlineError::TooLarge { bytes: 13, cap: 8 })
    );
}

#[cfg(feature = "rust")]
#[test]
fn node_budget_is_enforced() {
    let opts = OutlineOpts {
        node_budget: 5,
        ..OutlineOpts::default()
    };
    assert_eq!(
        outline(RUST, Lang::Rust, &opts),
        Err(OutlineError::Budget { budget: 5 })
    );
}

#[cfg(feature = "rust")]
#[test]
fn parse_timeout_is_enforced() {
    let big: String = (0..2000)
        .map(|i| format!("fn f{i}(a: u32) -> u32 {{\n    a + {i}\n}}\n"))
        .collect();
    let opts = OutlineOpts {
        parse_timeout: Duration::ZERO,
        ..OutlineOpts::default()
    };
    assert_eq!(
        outline(&big, Lang::Rust, &opts),
        Err(OutlineError::Timeout { ms: 0 })
    );
}

#[cfg(feature = "rust")]
#[test]
fn members_past_the_cap_are_hidden_but_still_symbols() {
    let opts = OutlineOpts {
        max_members: 2,
        ..OutlineOpts::default()
    };
    let o = outline(RUST, Lang::Rust, &opts).expect("outline");
    let (kept, hidden) = shape(&o);
    assert_eq!(kept, [(1, 4), (9, 19), (23, 28)]);
    assert_eq!(
        hidden,
        [hid(5, 8, "struct Foo"), hid(20, 22, "fn new"), top(29, 44)]
    );
    let names: Vec<&str> = o.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Foo", "Foo", "TABLE", "main"]);
}

#[cfg(feature = "rust")]
#[test]
fn shown_lines_counts_kept_rows() {
    let o = run(RUST, Lang::Rust);
    assert_eq!(o.shown_lines(), 4 + 11 + 10 + 9);
}
