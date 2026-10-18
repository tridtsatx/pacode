//! Per-language node-kind tables, one file per language family. Data only; kinds were
//! checked against each grammar's `node-types.json` for the pinned crate versions.
//! The tables are compiled in whatever the Cargo features: a table without its grammar
//! is a few hundred bytes, and keeping them unconditional keeps `Lang` matches exhaustive.

mod bash;
mod c;
mod csharp;
mod data;
mod go;
mod java;
mod js;
mod kotlin;
mod markdown;
mod php;
mod python;
mod ruby;
mod rust;
mod swift;

pub(crate) use bash::BASH;
pub(crate) use c::{C, CPP};
pub(crate) use csharp::CSHARP;
pub(crate) use data::{JSON, TOML, YAML};
pub(crate) use go::GO;
pub(crate) use java::JAVA;
pub(crate) use js::{JAVASCRIPT, TYPESCRIPT};
pub(crate) use kotlin::KOTLIN;
pub(crate) use markdown::MARKDOWN;
pub(crate) use php::PHP;
pub(crate) use python::PYTHON;
pub(crate) use ruby::RUBY;
pub(crate) use rust::RUST;
pub(crate) use swift::SWIFT;
