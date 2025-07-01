/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use cpython::*;
use cpython_ext::ResultPyErrExt;
use cpython_ext::convert::Serde;

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "serde"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add(py, "toml_loads", py_fn!(py, toml_loads(text: &str)))?;
    Ok(m)
}

fn toml_loads(py: Python, text: &str) -> PyResult<Serde<toml::Value>> {
    let value: toml::Value = toml::from_str(text).map_pyerr(py)?;
    Ok(Serde(value))
}
