// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The preprocessor.

use std::string::String;
use std::vec::Vec;

use crate::Version;
use crate::diag::Diagnostics;
use crate::intern::Interner;
use crate::pp::{self, Ext, ExtSet, PpKind, Preprocessed};

/// Preprocesses `sources` with every extension available; returns the
/// output tokens spelled and joined by spaces, the information log, and
/// the result.
fn run_strings(sources: &[&str]) -> (String, String, Preprocessed) {
    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let out = pp::preprocess(sources, ExtSet::all(), &mut interner, &mut diags);
    let text: Vec<String> = out
        .tokens
        .iter()
        .map(|t| match t.kind {
            PpKind::Ident(s) | PpKind::Number(s) => interner.get(s).into(),
            PpKind::Punct(p) => p.as_str().into(),
            PpKind::Invalid(c) => std::format!("<{c}>"),
            PpKind::Newline => "\\n".into(),
        })
        .collect();
    (text.join(" "), diags.log(), out)
}

fn run(source: &str) -> (String, String) {
    let (text, log, _) = run_strings(&[source]);
    (text, log)
}

/// Output of a shader that must preprocess without messages.
fn ok(source: &str) -> String {
    let (text, log) = run(source);
    assert!(log.is_empty(), "unexpected messages for {source:?}:\n{log}");
    text
}

/// The log of a shader that must fail; checks that it contains `needle`.
fn fails(source: &str, needle: &str) {
    let (_, log) = run(source);
    assert!(log.contains("ERROR"), "expected an error for {source:?}, log:\n{log}");
    assert!(log.contains(needle), "log for {source:?} lacks {needle:?}:\n{log}");
}

#[test]
fn object_and_function_macros_expand() {
    assert_eq!(ok("#define N 4\nfloat a[N];"), "float a [ 4 ] ;");
    assert_eq!(ok("#define SQ(x) ((x)*(x))\nSQ(a+1)"), "( ( a + 1 ) * ( a + 1 ) )");
    assert_eq!(ok("#define F(a, b) b a\nF((1, 2), [3])"), "[ 3 ] ( 1 , 2 )");
    assert_eq!(ok("#define E()\nx E() y"), "x y");
    assert_eq!(ok("#define ONE(x) <x>\nONE()"), "< >");
}

#[test]
fn a_function_like_macro_name_alone_is_an_identifier() {
    assert_eq!(ok("#define f(x) x\nf + f(1)"), "f + 1");
    // Whitespace before the parenthesis makes an object-like macro.
    assert_eq!(ok("#define g (x) x\ng"), "( x ) x");
}

#[test]
fn macros_do_not_expand_inside_their_own_expansion() {
    assert_eq!(ok("#define A A B\nA"), "A B");
    assert_eq!(ok("#define A B\n#define B A\nA B"), "A B");
    // A painted name stays unexpanded even when passed along later.
    assert_eq!(ok("#define f(x) g(x)\n#define g(x) f(x)\nf(1)"), "f ( 1 )");
}

#[test]
fn arguments_are_expanded_before_substitution() {
    assert_eq!(ok("#define X 7\n#define ID(a) a\nID(X)"), "7");
    assert_eq!(ok("#define TWICE(a) a a\n#define X 1\nTWICE(X)"), "1 1");
    assert_eq!(ok("#define ID(a) a\nID(ID(ID(3)))"), "3");
}

#[test]
fn an_invocation_may_span_lines() {
    let (text, log) = run("#define F(a,b) a+b\nF(1,\n2) x\ny");
    assert!(log.is_empty(), "{log}");
    assert_eq!(text, "1 + 2 x y");
    // But a directive inside the arguments is an error.
    fails("#define F(a) a\nF(1\n#define X\n)", "directive inside macro arguments");
}

#[test]
fn wrong_argument_counts_are_errors() {
    fails("#define F(a,b) a\nF(1)", "takes 2 argument(s), 1 given");
    fails("#define F(a) a\nF(1,2)", "takes 1 argument(s), 2 given");
    fails("#define F(a) a\nF(1", "unterminated macro invocation");
}

#[test]
fn predefined_macros() {
    assert_eq!(ok("__VERSION__ GL_ES GL_FRAGMENT_PRECISION_HIGH"), "100 1 1");
    assert_eq!(ok("#version 300 es\n__VERSION__"), "300");
    assert_eq!(ok("a\nb\n__LINE__"), "a b 3");
    assert_eq!(ok("GL_OES_standard_derivatives"), "1");
    // Extensions of GLSL ES 1.00 have no macro in 3.00.
    assert_eq!(ok("#version 300 es\nGL_OES_standard_derivatives"), "GL_OES_standard_derivatives");
    fails("#define GL_ES 2", "reserved");
    fails("#undef __LINE__", "predefined");
    fails("#define __VERSION__ 3", "predefined");
}

#[test]
fn line_numbers_survive_comments_and_continuations() {
    assert_eq!(ok("/* a\nb\nc */ __LINE__"), "3");
    assert_eq!(ok("// x\n__LINE__"), "2");
    // GLSL ES 3.00 joins lines ending in a backslash, but keeps counting.
    assert_eq!(ok("#version 300 es\nflo\\\nat __LINE__\n__LINE__"), "float 3 4");
    // GLSL ES 1.00 has no line continuation: the backslash is invalid.
    assert_eq!(ok("a \\\nb"), "a <\\> b");
}

#[test]
fn carriage_returns_are_newlines() {
    assert_eq!(ok("a\r\nb\rc\n\r__LINE__"), "a b c 4");
}

#[test]
fn line_directive_renumbers() {
    assert_eq!(ok("#line 10\n__LINE__\n__LINE__"), "10 11");
    assert_eq!(ok("#line 5 7\n__FILE__ __LINE__"), "7 5");
    let (_, log) = run("#line 40\n#error here");
    assert!(log.contains("0:40: #error here"), "{log}");
}

#[test]
fn source_strings_are_numbered_and_concatenated() {
    let (text, log, _) = run_strings(&["a __FILE__\n", "b __FILE__ __LINE__"]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(text, "a 0 b 1 1");
    // GL concatenates the strings: a token may span two of them.
    let (text, _, _) = run_strings(&["flo", "at x;"]);
    assert_eq!(text, "float x ;");
    // A zero byte ends its string.
    let (text, _, _) = run_strings(&["a\0junk", " b"]);
    assert_eq!(text, "a b");
}

#[test]
fn conditionals_select_groups() {
    let src = "#if 1 + 2 * 3 == 7 && (4 << 1) == 8\nyes\n#else\nno\n#endif";
    assert_eq!(ok(src), "yes");
    assert_eq!(ok("#define X\n#ifdef X\na\n#endif\n#ifndef X\nb\n#endif"), "a");
    assert_eq!(ok("#if 0\na\n#elif 0\nb\n#elif 2\nc\n#else\nd\n#endif"), "c");
    assert_eq!(ok("#if defined(X) || defined Y\na\n#else\nb\n#endif"), "b");
    assert_eq!(ok("#if 0\n#if garbage (\n#bogus directive\n@$\n#endif\n#endif\nz"), "z");
}

#[test]
fn undefined_names_in_if_are_errors_unless_not_evaluated() {
    fails("#if FOO\n#endif", "'FOO' is not defined");
    assert_eq!(ok("#if 0 && FOO\na\n#endif\n#if 1 || FOO / 0\nb\n#endif"), "b");
    fails("#if 1 / 0\n#endif", "division by zero");
    fails("#if 1.5\n#endif", "not an integer constant");
    fails("#if (1\n#endif", "unexpected end");
}

#[test]
fn elif_is_not_evaluated_after_a_taken_branch() {
    assert_eq!(ok("#if 1\na\n#elif UNDEFINED\nb\n#endif"), "a");
}

#[test]
fn unbalanced_conditionals_are_errors() {
    fails("#if 1\na", "unterminated #if");
    fails("#endif", "#endif without #if");
    fails("#else", "#else without #if");
    fails("#if 1\n#else\n#else\n#endif", "#else after #else");
    fails("#ifdef X Y\n#endif", "unexpected tokens after #ifdef");
}

#[test]
fn error_directive_fails_with_its_message() {
    fails("#error this shader is unhappy", "#error this shader is unhappy");
    // Not in a skipped group.
    assert_eq!(ok("#if 0\n#error no\n#endif\nok"), "ok");
}

#[test]
fn redefinition_must_be_identical() {
    assert_eq!(ok("#define A 1 + 2\n#define A 1   +   2\nA"), "1 + 2");
    fails("#define A 1\n#define A 2", "redefined differently");
    fails("#define F(a) a\n#define F(b) b", "redefined differently");
    assert_eq!(ok("#define A 1\n#undef A\n#define A 2\nA"), "2");
}

#[test]
fn version_directive() {
    let (_, log, out) = run_strings(&["#version 300 es\nvoid main(){}"]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(out.version, Version::V300);
    let (_, log, out) = run_strings(&["// a comment\n/* another */ #version 300 es\nx"]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(out.version, Version::V300);
    let (_, log, out) = run_strings(&["#version 100\nx"]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(out.version, Version::V100);
    assert_eq!(run_strings(&["x"]).2.version, Version::V100);
    fails("#version 310 es\n", "version 310 is not supported");
    fails("#version 300\n", "#version 300 es");
    fails("int x;\n#version 300 es\n", "#version must come before anything else");
    fails("#define A\n#version 100\n", "#version must come before anything else");
}

#[test]
fn extension_directive() {
    let (_, log, out) = run_strings(&["#extension GL_OES_standard_derivatives : enable\n"]);
    assert!(log.is_empty(), "{log}");
    assert!(out.enabled.contains(Ext::OesStandardDerivatives));
    let (_, log, out) = run_strings(&["#extension GL_EXT_frag_depth : warn\n"]);
    assert!(log.is_empty(), "{log}");
    assert!(out.enabled.contains(Ext::ExtFragDepth) && out.warn.contains(Ext::ExtFragDepth));
    fails("#extension GL_FOO_bar : require\n", "'GL_FOO_bar' is not supported");
    let (_, log) = run("#extension GL_FOO_bar : enable\n");
    assert!(log.contains("WARNING") && !log.contains("ERROR"), "{log}");
    fails("#extension all : enable\n", "'all' can only be used");
    fails("#extension GL_OES_standard_derivatives enable\n", "#extension must be");
    // After other tokens: an error in 3.00, a warning in 1.00.
    fails("#version 300 es\nint x;\n#extension all : warn\n", "must come before");
    let (_, log) = run("int x;\n#extension all : warn\n");
    assert!(log.contains("WARNING") && !log.contains("ERROR"), "{log}");
}

#[test]
fn macro_expansion_is_bounded() {
    let mut src = String::new();
    for i in 0..40 {
        src.push_str(&std::format!("#define M{} M{} M{}\n", i, i + 1, i + 1));
    }
    src.push_str("M0\n");
    fails(&src, "too many tokens");
}

#[test]
fn invariant_all_pragma_is_recorded() {
    let (_, log, out) = run_strings(&["#pragma STDGL invariant(all)\n#pragma whatever you like\n"]);
    assert!(log.is_empty(), "{log}");
    assert!(out.invariant_all);
}

#[test]
fn unterminated_comment_is_an_error() {
    fails("a /* never closed", "unterminated comment");
}

#[test]
fn invalid_directives_are_errors() {
    fails("#foo\n", "invalid preprocessor directive '#foo'");
    fails("# 12\n", "invalid preprocessor directive");
    assert_eq!(ok("#\nx"), "x");
}

#[test]
fn pp_numbers_are_whole_tokens() {
    assert_eq!(ok("1.0e-5 .5 0x1Fu 2.f 1e10"), "1.0e-5 .5 0x1Fu 2.f 1e10");
    assert_eq!(ok("0x1e+5"), "0x1e + 5");
}

#[test]
fn every_extension_is_known_by_name() {
    for e in Ext::ALL {
        assert!(e.name().starts_with("GL_"));
    }
}

#[test]
fn long_shaders_preprocess_in_linear_time() {
    // About 300 kB of source. Reading it used to look over the whole string
    // for every byte (minutes for this much); it should take a moment, even
    // unoptimised.
    let line = "float f(float x) { return x * 2.0 + 1.0; } // a comment to skip\n";
    let mut src = String::from("#version 300 es\n");
    for _ in 0..4500 {
        src.push_str(line);
    }
    let t = std::time::Instant::now();
    let (_, log, out) = run_strings(&[&src]);
    let secs = t.elapsed().as_secs_f64();
    assert!(log.is_empty(), "{log}");
    assert!(out.tokens.len() >= 4500 * 15);
    assert!(secs < 2.0, "{secs} s to preprocess {} bytes", src.len());
}

#[test]
fn a_replacement_is_counted_before_it_is_built() {
    // An argument of 100,000 tokens used 10,000 times would make a billion
    // tokens (tens of gigabytes) from 220 kB of source.
    let src = std::format!("#define F(x) {}\nF({})\n", "x ".repeat(10_000), "a ".repeat(100_000));
    let t = std::time::Instant::now();
    fails(&src, "too many tokens");
    assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
}

#[test]
fn expanded_arguments_count_against_the_budget() {
    // Each level doubles the argument: 2^24 tokens at the top.
    let mut src = String::from("#define D(x) x x\n");
    src.push_str(&"D(".repeat(24));
    src.push('a');
    src.push_str(&")".repeat(24));
    let t = std::time::Instant::now();
    fails(&src, "too many tokens");
    assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    // Under the budget it works.
    let src = std::format!("#define D(x) x x\n{}a{}", "D(".repeat(4), ")".repeat(4));
    assert_eq!(ok(&src), ["a"; 16].join(" "));
}

#[test]
fn long_chains_of_macros_expand_in_linear_time() {
    // Each macro expands to the next: 200,000 expansions are being
    // rescanned at the end. Telling whether a name is one of them must not
    // look through all of them (twenty billion steps).
    let n = 200_000;
    let mut src = String::new();
    for i in 0..n {
        src.push_str(&std::format!("#define M{i} M{}\n", i + 1));
    }
    src.push_str(&std::format!("#define M{n} M0 end\nM0\n"));
    let t = std::time::Instant::now();
    // The chain comes back to M0, which is not expanded again.
    assert_eq!(ok(&src), "M0 end");
    assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
}

#[test]
fn macros_with_many_parameters_are_linear() {
    // Duplicate parameters and parameter uses are found by lookup, not by
    // comparing every pair (tens of billions of steps for 200,000
    // parameters).
    let n = 200_000;
    let params: Vec<String> = (0..n).map(|i| std::format!("p{i}")).collect();
    let args: Vec<String> = (0..n).map(|i| std::format!("{}", i % 10)).collect();
    let src = std::format!("#define F({}) {}\nF({})\n", params.join(","), params.join(" "), args.join(","));
    let t = std::time::Instant::now();
    let out = ok(&src);
    assert_eq!(out.len(), 2 * n - 1);
    assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    fails("#define F(a, b, a) a\n", "duplicate macro parameter 'a'");
}

#[test]
fn deep_if_expressions_are_errors_not_crashes() {
    for (open, close) in [("- ", ""), ("!", ""), ("(", ")"), ("~(", ")")] {
        let src = std::format!("#if {}1{}\n#endif\n", open.repeat(100_000), close.repeat(100_000));
        let log = super::on_small_stack(move || run(&src).1);
        assert!(log.contains("nests parentheses or unary operators too deeply"), "{log}");
    }
    // Within the limit, nesting works.
    assert_eq!(ok(&std::format!("#if {}1{}\nyes\n#endif\n", "(".repeat(30), ")".repeat(30))), "yes");
    assert_eq!(ok(&std::format!("#line {}5\nx\n", "- - ".repeat(10))), "x");
}

#[test]
fn version_follows_any_amount_of_comments() {
    // A licence of 5 kB in a string of its own, then `#version 300 es`.
    let licence = std::format!("/*\n{}*/\n", "This file is distributed under a licence.\n".repeat(120));
    assert!(licence.len() > 4096);
    let (_, log, out) = run_strings(&[&licence, "#version 300 es\nuint u;\n"]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(out.version, Version::V300);
    // Line comments, in the same string, longer still.
    let comments = "// a comment that goes on for a while\n".repeat(400);
    let (_, log, out) = run_strings(&[&std::format!("{comments}\n  #  version 300 es\n")]);
    assert!(log.is_empty(), "{log}");
    assert_eq!(out.version, Version::V300);
    // A comment cut short, or other tokens first: no version (1.00).
    let (_, _, out) = run_strings(&["/* not closed #version 300 es"]);
    assert_eq!(out.version, Version::V100);
    let (_, log, out) = run_strings(&["x\n#version 300 es\n"]);
    assert_eq!(out.version, Version::V100);
    assert!(log.contains("#version must come before anything else"), "{log}");
    // A number too large for any version.
    let (_, _, out) = run_strings(&["#version 99999999999999999999 es\n"]);
    assert_eq!(out.version, Version::V100);
}
