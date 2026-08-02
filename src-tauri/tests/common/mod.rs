//! tests/ 共享测试工具。common 在子目录，cargo 不会把它当测试目标编译；
//! 各测试文件按需 `mod common;` 引用，单个文件未用到的 helper 属正常。
#![allow(dead_code)]

pub mod json_rpc;
pub mod oauth_mock;
