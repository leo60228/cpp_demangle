extern crate glob;

use glob::glob;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path;

fn get_crate_dir() -> io::Result<path::PathBuf> {
    Ok(path::PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "no CARGO_MANIFEST_DIR")
        })?,
    ))
}

fn get_out_dir() -> io::Result<path::PathBuf> {
    Ok(path::PathBuf::from(env::var("OUT_DIR")
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "no OUT_DIR"))?))
}

fn get_crate_test_path(file_name: &str) -> io::Result<path::PathBuf> {
    let mut test_path = get_crate_dir()?;
    test_path.push("tests");
    assert!(test_path.is_dir());
    test_path.push(file_name);
    Ok(test_path)
}

fn get_test_path(file_name: &str) -> io::Result<path::PathBuf> {
    let mut test_path = get_out_dir()?;
    assert!(test_path.is_dir());
    test_path.push(file_name);
    Ok(test_path)
}

/// Generate tests that ensure that we don't panic when parsing and demangling
/// the seed test cases that we pass to AFL.rs assert (including the failing
/// test cases historically found by AFL.rs).
fn generate_sanity_tests_from_afl_seeds() -> io::Result<()> {
    for entry in glob("./in/*").expect("should read glob pattern") {
        if let Ok(path) = entry {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-changed=tests/afl_seeds.rs");

    let test_path = get_test_path("afl_seeds.rs")?;
    let mut test_file = fs::File::create(test_path)?;

    writeln!(
        &mut test_file,
        "
extern crate cpp_demangle;
use std::fs;
use std::io::Read;
"
    )?;

    let mut in_dir = get_crate_dir()?;
    in_dir.push("in");
    assert!(in_dir.is_dir());

    let entries = fs::read_dir(in_dir)?;

    for entry in entries {
        let entry = entry?;

        let path = entry.path();
        let file_name = path.file_name().ok_or(io::Error::new(
            io::ErrorKind::Other,
            "no file name for AFL.rs seed test case",
        ))?;

        writeln!(
            &mut test_file,
            r#"
#[test]
fn test_afl_seed_{}() {{
    let mut file = fs::File::open("{}").unwrap();
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();
    let _ = cpp_demangle::Symbol::new(contents);
    assert!(true, "did not panic when parsing");
}}
"#,
            file_name.to_string_lossy(),
            path.to_string_lossy()
        )?;
    }

    Ok(())
}

// Ratcheting number that is increased as more libiberty tests start
// passing. Once they are all passing, this can be removed and we can enable all
// of them by default.
const LIBIBERTY_TEST_THRESHOLD: usize = 85;

/// Read `tests/libiberty-demangle-expected`, parse its input mangled symbols,
/// and expected output demangled symbols, and generate test cases for them.
///
/// We do not support all of the options that the libiberty demangler does,
/// therefore we skip tests that use options we do not intend to
/// support. Basically, we only support `--format=gnu-v3` (which is the System V
/// C++ ABI), and none of the legacy C/C++ compiler formats, nor Java/D/etc
/// language symbol mangling.
fn generate_compatibility_tests_from_libiberty() -> io::Result<()> {
    println!("cargo:rerun-if-changed=tests/libiberty-demangle-expected");

    let test_path = get_test_path("libiberty.rs")?;
    let _ = fs::remove_file(&test_path);
    let mut test_file = fs::File::create(test_path)?;

    writeln!(
        &mut test_file,
        "
extern crate cpp_demangle;
extern crate diff;
use std::fmt::Write;
"
    )?;

    let libiberty_tests = get_crate_test_path("libiberty-demangle-expected")?;
    let libiberty_tests = fs::File::open(libiberty_tests)?;
    let libiberty_tests = io::BufReader::new(libiberty_tests);

    let mut lines = libiberty_tests.lines().filter(|line| {
        line.as_ref().map(|l| !l.starts_with('#')).unwrap_or(true)
    });

    let mut n = 0;

    loop {
        let options = match lines.next() {
            None => break,
            Some(Ok(line)) => line,
            Some(Err(e)) => return Err(e),
        };

        let mangled = match lines.next() {
            Some(Ok(line)) => line,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "expected a line with a mangled symbol",
                ))
            }
            Some(Err(e)) => return Err(e),
        };

        let demangled = match lines.next() {
            Some(Ok(line)) => line,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "expected a line with the demangled symbol",
                ))
            }
            Some(Err(e)) => return Err(e),
        };

        if options.find("--no-params").is_some() {
            // This line is the expected demangled output without function and
            // template parameters, but we don't currently have such an option
            // in `cpp_demangle`, so just consume and ignore the line.
            match lines.next() {
                Some(Ok(_)) => {}
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "expected a line with the demangled symbol without parameters",
                    ))
                }
                Some(Err(e)) => return Err(e),
            }
        }

        // Skip tests for unsupported languages or options.
        if options.find("--format=gnu-v3").is_none() || options.find("--is-v3-ctor").is_some()
            || options.find("--is-v3-dtor").is_some()
            || options.find("--ret-postfix").is_some()
        {
            continue;
        }

        let cfg = if n <= LIBIBERTY_TEST_THRESHOLD {
            ""
        } else {
            r###"#[cfg(feature = "run_libiberty_tests")]"###
        };

        writeln!(
            test_file,
            r###"
{}
#[test]
fn test_libiberty_demangle_{}_() {{
    let mangled = br#"{}"#;
    let mangled_str = String::from_utf8_lossy(mangled).into_owned();
    println!("Parsing mangled symbol: {{}}", mangled_str);

    let expected = r#"{}"#;

    let sym = match cpp_demangle::Symbol::new(&mangled[..]) {{
        Ok(sym) => sym,
        Err(_) if mangled_str == expected => return,
        Err(e) => panic!("Should parse mangled symbol {{}}", e),
    }};

    let mut actual = String::new();
    if let Err(e) = write!(&mut actual, "{{}}", sym) {{
        panic!("Error while demangling '{{}}': {{}}",
               mangled_str,
               e);
    }}

    println!("     Expect demangled symbol: {{}}", expected);
    println!("Actually demangled symbol as: {{}}", actual);

    if expected != actual {{
        println!("");
        println!("Diff:");
        println!("--- expected");
        print!("+++ actual");

        let mut last = None;
        for cmp in diff::chars(expected, &actual) {{
            match (last, cmp.clone()) {{
                (Some(diff::Result::Left(_)), diff::Result::Left(_)) |
                (Some(diff::Result::Both(..)), diff::Result::Both(..)) |
                (Some(diff::Result::Right(_)), diff::Result::Right(_)) => {{}}

                (_, diff::Result::Left(_))  => print!("\n-"),
                (_, diff::Result::Both(..))  => print!("\n "),
                (_, diff::Result::Right(_)) => print!("\n+"),
            }};
            match cmp.clone() {{
                diff::Result::Left(c) |
                diff::Result::Both(c, _) |
                diff::Result::Right(c) => print!("{{}}", c),
            }}
            last = Some(cmp);
        }}
        println!("");
    }}

    assert_eq!(expected, actual);
}}
"###,
            cfg,
            n,
            mangled.trim(),
            demangled.trim()
        )?;

        n += 1;
    }

    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    generate_sanity_tests_from_afl_seeds()
        .expect("should generate sanity tests from AFL.rs seed test cases");

    generate_compatibility_tests_from_libiberty()
        .expect("should generate compatibility tests from tests/libiberty-demangle-expected");
}
