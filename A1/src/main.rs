use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
};
use wasmer::{
    Function, FunctionEnv, FunctionEnvMut, Instance, Memory, Module, Store, Value, imports,
};

#[derive(Clone)]
struct AllocatorEnv {
    memory: Option<Memory>,
    heap_ptr: Arc<Mutex<u32>>,
}

fn eta_malloc(env: FunctionEnvMut<AllocatorEnv>, size: u32) -> u32 {
    let mut heap = env.data().heap_ptr.lock().unwrap();
    let memory = env.data().memory.as_ref().expect("Memory not initialized");
    *heap = (*heap + 7) & !7;
    let ptr = *heap;
    let new_ptr = ptr.checked_add(size).expect("heap overflow");
    let mem_bytes = memory.view(&env).data_size() as usize;
    if (new_ptr as usize) >= mem_bytes {
        panic!(
            "Out of memory: requested {} bytes, only {} available",
            size,
            mem_bytes - ptr as usize
        );
    }
    *heap = new_ptr;
    ptr
}

fn run_wasm(src_path: &Path, opt: bool) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let pipeline = opt.then(rwetac::setting::Pipeline::default_full);
    rwetac::compile::compile(
        rwetac::setting::Stage::Codegen,
        false,
        pipeline,
        false,
        None,
        src_path,
    )?;

    let wat_path = src_path.with_extension("wat");
    let wat_code = fs::read_to_string(&wat_path);
    let _ = fs::remove_file(&wat_path);
    let wat_code = wat_code?;

    let mut store = Store::default();
    let module = Module::new(&store, &wat_code)?;

    let heap_ptr = Arc::new(Mutex::new(0u32));
    let env = FunctionEnv::new(
        &mut store,
        AllocatorEnv {
            memory: None,
            heap_ptr,
        },
    );
    let eta_malloc_func = Function::new_typed_with_env(&mut store, &env, eta_malloc);

    let import_object = imports! {
        "env" => {
            "eta_malloc" => eta_malloc_func,
        }
    };

    let instance = Instance::new(&mut store, &module, &import_object)?;

    let heap_base = instance
        .exports
        .get_global("__heap_base")?
        .get(&mut store)
        .i32()
        .expect("`__heap_base` global not found or not an i32");

    let memory = instance.exports.get_memory("memory")?.clone();
    {
        let mut env_mut = env.into_mut(&mut store);
        env_mut.data_mut().memory = Some(memory);
        *env_mut.data_mut().heap_ptr.lock().unwrap() = heap_base as u32;
    }

    let main_func = instance.exports.get_function("main")?;
    let results = main_func.call(&mut store, &[])?;

    Ok(results.to_vec())
}

fn is_trap(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<wasmer::RuntimeError>().is_some()
}

#[derive(Debug, PartialEq)]
enum Expectation {
    Value(i64),
    Trap,
}

fn expected_value(src_path: &Path) -> Result<Expectation, String> {
    let src = fs::read_to_string(src_path)
        .map_err(|e| format!("could not read {}: {e}", src_path.display()))?;
    for line in src.lines() {
        if let Some(rest) = line.trim().strip_prefix("// EXPECT:") {
            let rest = rest.trim();
            if rest == "trap" {
                return Ok(Expectation::Trap);
            }
            return rest
                .parse::<i64>()
                .map(Expectation::Value)
                .map_err(|e| format!("malformed EXPECT directive {rest:?}: {e}"));
        }
    }
    Err("no `// EXPECT: <int>` or `// EXPECT: trap` directive found".to_string())
}

fn as_i64(v: &Value) -> Result<i64, String> {
    match v {
        Value::I32(i) => Ok(*i as i64),
        Value::I64(i) => Ok(*i),
        other => Err(format!("expected an integer result, got {other:?}")),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::I32(_) => "i32",
        Value::I64(_) => "i64",
        _ => "other",
    }
}

fn resolve_example_path(path: &Path) -> PathBuf {
    if let Ok(content) = fs::read_to_string(path) {
        let trimmed = content.trim();
        if trimmed.starts_with("../") && !trimmed.contains('\n')
            && let Some(parent) = path.parent() {
                let candidate = parent.join(trimmed);
                if candidate.is_file() {
                    return candidate;
                }
            }
    }
    path.to_path_buf()
}

fn run_single_example(path: &Path) -> Result<String, String> {
    let resolved = resolve_example_path(path);
    let path = resolved.as_path();
    let expected = expected_value(path)?;
    let unopt_res = run_wasm(path, false);
    let opt_res = run_wasm(path, true);

    match expected {
        Expectation::Trap => {
            for (label, res) in [("unoptimised", &unopt_res), ("optimised", &opt_res)] {
                match res {
                    Ok(v) => return Err(format!("expected a trap ({label}), returned {v:?}")),
                    Err(e) if !is_trap(e.as_ref()) => {
                        return Err(format!("expected a trap ({label}), failed instead: {e}"));
                    }
                    Err(_) => {}
                }
            }
            Ok("unopt: trap, opt: trap (expected: trap)".to_string())
        }
        Expectation::Value(expected_val) => {
            let unopt = unopt_res.map_err(|e| format!("unoptimised run failed: {e}"))?;
            let opt = opt_res.map_err(|e| format!("optimised run failed: {e}"))?;

            if unopt.len() != 1 {
                return Err(format!(
                    "expected 1 return value (unoptimised), got {unopt:?}"
                ));
            }
            if opt.len() != 1 {
                return Err(format!("expected 1 return value (optimised), got {opt:?}"));
            }

            let unopt_val = as_i64(&unopt[0])?;
            let opt_val = as_i64(&opt[0])?;

            if unopt_val != expected_val {
                return Err(format!(
                    "wrong result unoptimised: got {unopt_val}, expected {expected_val}"
                ));
            }
            if opt_val != expected_val {
                return Err(format!(
                    "wrong result optimised: got {opt_val}, expected {expected_val}"
                ));
            }
            if type_name(&unopt[0]) != type_name(&opt[0]) {
                return Err(format!(
                    "optimiser changed result type: {} vs {}",
                    type_name(&unopt[0]),
                    type_name(&opt[0])
                ));
            }

            Ok(format!(
                "unopt: {unopt_val}, opt: {opt_val} (expected: {expected_val})"
            ))
        }
    }
}

fn find_examples_dir() -> anyhow::Result<PathBuf> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples"),
        PathBuf::from("examples"),
        PathBuf::from("A1").join("examples"),
    ];
    for candidate in candidates {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Could not find `examples` directory")
}

fn collect_eta_files() -> anyhow::Result<Vec<PathBuf>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut files = Vec::new();

    if args.is_empty() {
        let dir = find_examples_dir()?;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("eta") {
                files.push(path);
            }
        }
    } else {
        for arg in args {
            let path = PathBuf::from(arg);
            if path.is_dir() {
                for entry in fs::read_dir(&path)? {
                    let p = entry?.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("eta") {
                        files.push(p);
                    }
                }
            } else {
                files.push(path);
            }
        }
    }

    files.sort();
    if files.is_empty() {
        anyhow::bail!("No .eta example files found");
    }
    Ok(files)
}

fn main() -> ExitCode {
    let files = match collect_eta_files() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("Running {} Eta example(s)...\n", files.len());

    let mut passed = 0usize;
    let mut failed = 0usize;

    for path in &files {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        match run_single_example(path) {
            Ok(summary) => {
                println!("  [PASS] {name:<36} {summary}");
                passed += 1;
            }
            Err(err) => {
                eprintln!("  [FAIL] {name:<36} {err}");
                failed += 1;
            }
        }
    }

    println!("\nSummary: {passed} passed, {failed} failed.");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_pass() {
        let dir = find_examples_dir().expect("examples directory not found");
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("failed to read examples directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("eta"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no .eta files found in {}", dir.display());

        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy();
            if let Err(err) = run_single_example(path) {
                panic!("{name} failed: {err}");
            }
        }
    }
}
