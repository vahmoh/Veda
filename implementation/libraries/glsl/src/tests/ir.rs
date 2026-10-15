// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linking and the SSA form: every program links, and its IR satisfies the
//! verifier's invariants.

use std::format;
use std::string::String;

use crate::ir::print::print;
use crate::link::Bindings;
use crate::program::{Program, link};
use crate::{Options, Stage, compile};

pub(crate) fn program(vs: &str, fs: &str) -> Program {
    let o = Options::default();
    let v = compile(Stage::Vertex, &[vs], &o);
    assert!(v.log.is_empty(), "vertex shader:\n{vs}\n{}", v.log);
    let f = compile(Stage::Fragment, &[fs], &o);
    assert!(f.log.is_empty(), "fragment shader:\n{fs}\n{}", f.log);
    let r = link(&v.shader.unwrap(), &f.shader.unwrap(), &Bindings::default(), &o.limits);
    assert!(r.log.is_empty(), "link:\n{}", r.log);
    r.program.expect("linked")
}

pub(crate) fn link_log(vs: &str, fs: &str) -> String {
    let o = Options::default();
    let v = compile(Stage::Vertex, &[vs], &o).shader.expect("vs");
    let f = compile(Stage::Fragment, &[fs], &o).shader.expect("fs");
    link(&v, &f, &Bindings::default(), &o.limits).log
}

const FS100: &str = "precision mediump float; void main() { gl_FragColor = vec4(1.0); }";
const FS300: &str = "#version 300 es\nprecision mediump float; out vec4 c; void main() { c = vec4(1.0); }";

fn vs_only(body: &str) -> Program {
    program(&format!("#version 300 es\n{body}"), FS300)
}

#[test]
fn a_simple_program_prints() {
    let p = program("attribute vec4 p; uniform float k; void main() { gl_Position = p * k; }", FS100);
    let text = print(&p.vertex);
    assert!(text.contains("input 0.x"), "{text}");
    assert!(text.contains("uniform 0.x"), "{text}");
    assert!(text.contains("gl_Position.w ="), "{text}");
    assert!(!text.contains("phi"), "no φ expected:\n{text}");
}

#[test]
fn loops_with_breaks_and_continues() {
    vs_only(
        r#"
        uniform int n;
        out float r;
        void main() {
            float acc = 0.0;
            for (int i = 0; i < 10; i++) {
                if (i == n) continue;
                for (int j = 0; j < i; ++j) {
                    if (j > 3) break;
                    acc += float(j);
                    if (acc > 100.0) { acc = -1.0; break; }
                }
                if (acc < 0.0) break;
            }
            int k = 0;
            do { k += 2; if (k == 6) continue; acc += 1.0; } while (k < n);
            while (bool b = k > 0) { k--; }
            r = acc;
            gl_Position = vec4(acc);
        }
    "#,
    );
}

#[test]
fn returns_inside_loops_and_branches() {
    let p = vs_only(
        r#"
        uniform int n;
        float f(int x) {
            for (int i = 0; i < 8; i++) {
                for (int j = 0; j < 8; j++) {
                    if (i * j == x) return float(i + j);
                }
                if (i == 7) return -1.0;
            }
            return 0.0;
        }
        float g(float y) { if (y > 0.0) return y; else return -y; }
        void main() { gl_Position = vec4(f(n), g(float(n)), 0.0, 1.0); }
    "#,
    );
    let text = print(&p.vertex);
    assert!(text.contains("loop"), "{text}");
}

#[test]
fn switch_with_fallthrough_and_continue() {
    vs_only(
        r#"
        uniform int n;
        void main() {
            float acc = 0.0;
            for (int i = 0; i < 4; i++) {
                switch (n + i) {
                    case 0: acc += 1.0;
                    case 1: acc += 2.0; break;
                    case 2: continue;
                    default: acc -= 1.0;
                    case 5: acc *= 2.0;
                }
                acc += 0.5;
            }
            gl_Position = vec4(acc);
        }
    "#,
    );
}

#[test]
fn out_parameters_and_side_effects() {
    vs_only(
        r#"
        uniform int n;
        void twice(inout float x, out float y) { y = x; x *= 2.0; }
        void main() {
            float a[4];
            int i = 0;
            a[0] = 1.0; a[1] = 2.0; a[2] = 3.0; a[3] = 4.0;
            twice(a[i++], a[i++]);
            float t;
            float m = modf(a[n & 3], t);
            bool b = (n > 2) && (++i > 3);
            gl_Position = vec4(a[0], a[1], m + t, b ? 1.0 : 0.0);
        }
    "#,
    );
}

#[test]
fn dynamic_indexing_everywhere() {
    let p = program(
        r#"#version 300 es
        uniform int n;
        uniform vec4 colors[8];
        uniform Block { vec4 v[4]; mat3 m[2]; } blk;
        struct S { float f[3]; vec2 g; };
        uniform S s[2];
        out vec4 o[2];
        void main() {
            vec4 local[3] = vec4[](vec4(1.0), vec4(2.0), vec4(3.0));
            local[n % 3].y = 5.0;
            o[n & 1] = colors[n] + blk.v[n] + vec4(blk.m[n & 1][n % 3], s[n & 1].f[n % 3]) + local[n % 3];
            o[1 - (n & 1)] = vec4(s[n & 1].g, 0.0, 0.0);
            gl_Position = vec4(1.0);
        }
    "#,
        r#"#version 300 es
        precision mediump float;
        in vec4 o[2];
        uniform int k;
        out vec4 c;
        void main() { c = o[k & 1]; }
    "#,
    );
    let text = print(&p.vertex);
    assert!(text.contains("uniform") && text.contains('['), "an indexed uniform load:\n{text}");
    assert!(text.contains("block"), "{text}");
    assert!(text.contains("select"), "{text}");
}

#[test]
fn textures_and_derivatives() {
    program(
        r#"#version 300 es
        in vec4 p; out vec2 uv;
        uniform sampler2D vt;
        void main() { uv = p.xy; gl_Position = p + textureLod(vt, p.xy, 0.0) + texture(vt, p.zw); }
    "#,
        r#"#version 300 es
        precision mediump float;
        precision highp sampler2DShadow;
        in vec2 uv;
        uniform sampler2D t[2];
        uniform sampler2DShadow sh;
        uniform samplerCube cube;
        out vec4 c;
        void main() {
            vec4 a = texture(t[1], uv) + textureProj(t[0], vec3(uv, 2.0)) + textureGrad(t[0], uv, dFdx(uv), dFdy(uv));
            float d = texture(sh, vec3(uv, 0.5)) + textureProjOffset(sh, vec4(uv, 0.5, 1.0), ivec2(1, 2));
            c = a * d + texture(cube, vec3(uv, 1.0), 1.0) + vec4(fwidth(uv), vec2(textureSize(t[0], 0)));
        }
    "#,
    );
}

#[test]
fn es2_programs_link() {
    program(
        r#"
        attribute vec3 pos;
        attribute mat2 rot;
        uniform mat4 mvp;
        varying vec2 v;
        varying float w[2];
        void main() { v = rot * pos.xy; w[0] = pos.z; w[1] = 1.0; gl_Position = mvp * vec4(pos, 1.0); gl_PointSize = 4.0; }
    "#,
        r#"
        #extension GL_OES_standard_derivatives : enable
        precision mediump float;
        varying vec2 v;
        varying float w[2];
        uniform sampler2D tex;
        void main() {
            vec4 c = texture2D(tex, v) * w[1] + texture2DProj(tex, vec4(v, 0.0, 2.0));
            gl_FragColor = c + vec4(dFdx(v.x), dFdy(v.y), gl_FragCoord.xy * 0.001);
            if (gl_FragColor.a < 0.0) discard;
        }
    "#,
    );
}

#[test]
fn link_errors() {
    let vs = "attribute vec4 p; void main() { gl_Position = p; }";
    let log =
        link_log(vs, "precision mediump float; varying vec2 v; void main() { gl_FragColor = vec4(v, 0.0, 1.0); }");
    assert!(log.contains("'v' is not declared by the vertex shader"), "{log}");
    let log = link_log(
        "varying vec3 v; void main() { v = vec3(1.0); gl_Position = vec4(0.0); }",
        "precision mediump float; varying vec2 v; void main() { gl_FragColor = vec4(v, 0.0, 1.0); }",
    );
    assert!(log.contains("is a 'vec3' in the vertex shader and a 'vec2'"), "{log}");
    let log = link_log(
        "uniform float u; void main() { gl_Position = vec4(u); }",
        "precision mediump float; uniform int u; void main() { gl_FragColor = vec4(float(u)); }",
    );
    assert!(log.contains("uniform 'u' has different types"), "{log}");
    let log = link_log(
        "#version 300 es\nflat out int v; void main() { v = 1; gl_Position = vec4(0.0); }",
        "#version 300 es\nprecision mediump float; in float v; out vec4 c; void main() { c = vec4(v); }",
    );
    assert!(log.contains("'v' is a 'int'"), "{log}");
    let log = link_log(
        "#version 300 es\nout float v; void main() { v = 1.0; gl_Position = vec4(0.0); }",
        "#version 300 es\nprecision mediump float; flat in float v; out vec4 c; void main() { c = vec4(v); }",
    );
    assert!(log.contains("different interpolation qualifiers"), "{log}");
    let log = link_log("#version 300 es\nvoid main() { gl_Position = vec4(0.0); }", FS100);
    assert!(log.contains("different GLSL ES versions"), "{log}");
}

#[test]
fn reflection_of_uniforms_and_attributes() {
    let p = program(
        r#"#version 300 es
        layout(location = 3) in vec4 a;
        in mat2 b;
        in float c;
        struct L { vec3 dir; float k[2]; };
        uniform L lights[2];
        uniform mat3 m;
        uniform float scalars[4];
        layout(std140) uniform Blk { vec3 x; float y; mat2 z; float arr[2]; } blk;
        void main() { gl_Position = a + vec4(b[0], c, lights[1].k[1]) + vec4(m[2], scalars[3]) + vec4(blk.x, blk.y); }
    "#,
        FS300,
    );
    let l = &p.linked;
    let names: std::vec::Vec<(&str, u32)> = l.attributes.iter().map(|a| (a.name.as_str(), a.location)).collect();
    assert_eq!(names, [("b", 0), ("c", 2), ("a", 3)]);
    let unames: std::vec::Vec<&str> = l.uniforms.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        unames,
        [
            "lights[0].dir",
            "lights[0].k[0]",
            "lights[1].dir",
            "lights[1].k[0]",
            "m",
            "scalars[0]",
            "Blk.x",
            "Blk.y",
            "Blk.z",
            "Blk.arr[0]"
        ]
    );
    // The API names block members by the block's name, not the
    // instance's (GLSL ES 3.00 section 4.3.7).
    // std140: vec3 at 0, float packs at 12, mat2 at 16 (two 16-byte
    // columns), float[2] at 48 with a 16-byte stride; block size 80.
    let off: std::vec::Vec<u32> = l.uniforms[6..].iter().map(|u| u.block.unwrap().offset).collect();
    assert_eq!(off, [0, 12, 16, 48]);
    assert_eq!(l.uniforms[9].block.unwrap().array_stride, 16);
    assert_eq!(l.blocks[0].size, 80);
}

/// Compiles and links on a thread with a small stack, within `secs`
/// seconds; returns the logs (and whether it linked).
fn bounded_link(vs: String, fs: String, secs: f64) -> (String, bool) {
    super::on_small_stack(move || {
        let t = std::time::Instant::now();
        let o = Options::default();
        let v = compile(Stage::Vertex, &[&vs], &o);
        let f = compile(Stage::Fragment, &[&fs], &o);
        let mut log = format!("{}{}", v.log, f.log);
        let mut linked = false;
        if let (Some(v), Some(f)) = (v.shader, f.shader) {
            let r = link(&v, &f, &Bindings::default(), &o.limits);
            log.push_str(&r.log);
            if let Some(p) = r.program {
                linked = true;
                crate::interp::compile(&p.vertex);
                crate::interp::compile(&p.fragment);
                crate::tgsi::translate(&p.vertex, &p.linked, &crate::tgsi::Options::default());
                crate::tgsi::translate(&p.fragment, &p.linked, &crate::tgsi::Options::default());
            }
        }
        let elapsed = t.elapsed().as_secs_f64();
        assert!(elapsed < secs, "{elapsed} s:\n{log}");
        (log, linked)
    })
}

#[test]
fn indices_constant_only_once_inlined_are_clamped() {
    // The index is constant only once `p` is inlined, so the checks never
    // saw it: it is clamped to the array, as an index computed at run time
    // is, and nothing reads outside the variable.
    for call in ["p(7)", "p(-1)", "p(1000000000)"] {
        let vs = format!(
            "#version 300 es\nuniform vec4 u[4]; uniform int k;\nstruct S {{ float a; vec2 b; }}; uniform S s[3];\n\
             uniform B {{ vec4 v[2]; mat2 m[2]; }} blk;\n\
             float p(int i) {{ return u[i][k] + u[i].y + s[i].b.y + blk.v[i][k] + blk.m[i][1].y; }}\n\
             void main() {{ gl_Position = vec4({call}); }}"
        );
        let (log, linked) = bounded_link(vs, FS300.into(), 2.0);
        assert!(linked, "{log}");
    }
}

#[test]
fn inlining_is_bounded() {
    // Forty functions, each calling the one before twice: inlined, 2^40
    // copies of the first.
    let mut src = String::from("#version 300 es\nfloat f0(float x) { return x + 1.0; }\n");
    for k in 1..=40 {
        src.push_str(&format!("float f{k}(float x) {{ return f{}(x) + f{}(x * 0.5); }}\n", k - 1, k - 1));
    }
    src.push_str("void main() { gl_Position = vec4(f40(1.0)); }");
    let (log, linked) = bounded_link(src, FS300.into(), 5.0);
    assert!(!linked && log.contains("too large once its function calls are inlined"), "{log}");
    // A chain of 20,000 functions, each calling the next: as deep as long.
    let mut src = String::from("#version 300 es\nfloat f0(float x) { return x; }\n");
    for k in 1..20_000 {
        src.push_str(&format!("float f{k}(float x) {{ return f{}(x); }}\n", k - 1));
    }
    src.push_str("void main() { gl_Position = vec4(f19999(1.0)); }");
    let (log, linked) = bounded_link(src, FS300.into(), 10.0);
    assert!(!linked && log.contains("nest too deeply once calls are inlined"), "{log}");
    // Functions of 100 nested ifs, each called inside the next's.
    let mut src = String::from("#version 300 es\nfloat f0(float a) { return a; }\n");
    for k in 1..=4 {
        src.push_str(&format!(
            "float f{k}(float a) {{ {} a = f{}(a) + 1.0; return a; }}\n",
            "if (a > 0.0) ".repeat(100),
            k - 1
        ));
    }
    src.push_str("void main() { gl_Position = vec4(f4(1.0)); }");
    let (log, linked) = bounded_link(src, FS300.into(), 5.0);
    assert!(!linked && log.contains("nest too deeply once calls are inlined"), "{log}");
}

#[test]
fn huge_private_arrays_are_refused_or_bounded() {
    // A private array is a value per component: its size is bounded, and
    // so is the code dynamic indices into it make.
    let vs =
        "#version 300 es\nuniform int k; void main() { float a[65536]; a[k] = 1.0; gl_Position = vec4(a[k + 1]); }";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 2.0);
    assert!(!linked && log.contains("too large for a variable"), "{log}");
    let vs = "#version 300 es\nuniform int k; float g[16384]; float h[16384]; float i[16384]; float j[16384]; float l[16384];\n\
              void main() { g[k] = h[k] + i[k] + j[k] + l[k]; gl_Position = vec4(g[0]); }";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 2.0);
    assert!(!linked && log.contains("come to more than 65536 components"), "{log}");
    // Ten lines that once made hundreds of thousands of instructions: a
    // loop writing and reading a large array through a computed index.
    let vs = "#version 300 es\nuniform int n;\nvoid main() {\n  float a[4096];\n  for (int i = 0; i < n; i++) a[i] = float(i);\n\
              float s = 0.0;\n  for (int i = 0; i < n; i++) s += a[i] * a[n - i];\n  gl_Position = vec4(s);\n}";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 5.0);
    assert!(linked || log.contains("too large"), "{log}");
}

#[test]
fn loops_get_phis_only_for_what_they_change() {
    // Two hundred variables in scope, a loop that changes one: a φ for it
    // alone, not for every component of every variable.
    let mut decls = String::new();
    let mut sum = String::new();
    for k in 0..200 {
        decls.push_str(&format!("vec4 v{k} = vec4(float({k}) * float(n));\n"));
        sum.push_str(&format!("t += v{k}.x;\n"));
    }
    let p = vs_only(&format!(
        "uniform int n;\nvoid main() {{\n{decls}float s = 0.0;\nfor (int i = 0; i < n; i++) s += 1.0;\nfloat t = s;\n{sum}gl_Position = vec4(t);\n}}"
    ));
    let phis = p.vertex.insts.iter().filter(|i| i.op == crate::ir::InstOp::Phi).count();
    assert!(phis <= 4, "{phis} φs:\n{}", print(&p.vertex));
}

#[test]
fn types_are_sized_once_and_bounded() {
    // Forty structures, each of two of the one before: 2^40 members, which
    // telling whether one holds a sampler once visited one by one.
    let mut src = String::from("#version 300 es\nstruct S0 { float a; };\n");
    for k in 1..=40 {
        src.push_str(&format!("struct S{k} {{ S{} a; S{} b; }};\n", k - 1, k - 1));
    }
    src.push_str("uniform S40 u;\nvoid main() { gl_Position = vec4(1.0); }");
    let (log, linked) = bounded_link(src, FS300.into(), 2.0);
    assert!(!linked && log.contains("is too large"), "{log}");
    // Forty structures, each of one of the one before: small, but as deep.
    let mut src = String::from("#version 300 es\nstruct S0 { float a; };\n");
    for k in 1..=40 {
        src.push_str(&format!("struct S{k} {{ S{} a; }};\n", k - 1));
    }
    src.push_str("uniform S40 u;\nvoid main() { gl_Position = vec4(1.0); }");
    let (log, linked) = bounded_link(src, FS300.into(), 2.0);
    assert!(!linked && log.contains("nested more than 32 deep"), "{log}");
    // 65536 x 65536 components: once 0 in 32 bits.
    let vs = "#version 300 es\nstruct S { float a[65536]; };\nuniform S u[65536];\nvoid main() { gl_Position = vec4(u[1].a[2]); }";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 2.0);
    assert!(!linked && log.contains("is too large"), "{log}");
    let vs = "#version 300 es\nuniform B { float a[65536]; } b[64];\nvoid main() { gl_Position = vec4(b[1].a[2]); }";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 2.0);
    assert!(!linked && log.contains("is too large"), "{log}");
    // Within the limits, a uniform of many leaves is checked before any is
    // listed (and fails).
    let vs = "#version 300 es\nstruct S { float a[256]; vec4 b[64]; };\nuniform S u[100];\nvoid main() { gl_Position = vec4(u[1].a[2]) + u[3].b[4]; }";
    let (log, linked) = bounded_link(vs.into(), FS300.into(), 2.0);
    assert!(!linked && log.contains("too many uniforms"), "{log}");
}

#[test]
fn constants_built_of_constants_are_bounded() {
    // Each constant twice the one before: unbounded memory from a few
    // hundred bytes.
    let mut src = String::from("#version 300 es\nconst float c0[4096] = float[4096](");
    src.push_str(&std::vec!["1.0"; 4096].join(","));
    src.push_str(");\n");
    for k in 1..=40 {
        src.push_str(&format!("const float c{k}[4096] = c{}; const float d{k}[4096] = c{};\n", k - 1, k - 1));
    }
    for k in 0..400 {
        src.push_str(&format!("struct T{k} {{ float a[4096]; float b[4096]; }}; const T{k} t{k} = T{k}(c40, d40);\n"));
    }
    src.push_str("void main() { gl_Position = vec4(t0.a[1]); }");
    let (log, linked) = bounded_link(src, FS300.into(), 5.0);
    assert!(!linked && log.contains("constant expressions come to more than"), "{log}");
}

#[test]
fn uniforms_and_varyings_are_counted_as_appendix_a_packs_them() {
    // An array occupies rows of one column: a thousand floats need a
    // thousand rows, more than the 256 there are.
    let log = link_log("uniform float a[1024]; void main() { gl_Position = vec4(a[1]); }", FS100);
    assert!(log.contains("too many uniforms in the vertex shader"), "{log}");
    // Two hundred floats beside a hundred vec3s fit in 200 rows.
    let log =
        link_log("uniform float a[200]; uniform vec3 b[100]; void main() { gl_Position = vec4(b[1], a[1]); }", FS100);
    assert!(log.is_empty(), "{log}");
    // Varyings: float[16] cannot fit in 15 rows.
    let log = link_log(
        "varying float v[16]; void main() { v[0] = 1.0; gl_Position = vec4(1.0); }",
        "precision mediump float; varying float v[16]; void main() { gl_FragColor = vec4(v[1]); }",
    );
    assert!(log.contains("too many varyings"), "{log}");
    // Mats, vec4s, vec3s with scalars beside them, vec2s in pairs.
    let log = link_log(
        "varying mat2 m; varying vec4 a[4]; varying vec3 b[4]; varying vec2 c[4]; varying float d[4];\n\
         void main() { m = mat2(1.0); a[0] = vec4(1.0); b[0] = vec3(1.0); c[0] = vec2(1.0); d[0] = 1.0; gl_Position = vec4(1.0); }",
        "precision mediump float; varying mat2 m; varying vec4 a[4]; varying vec3 b[4]; varying vec2 c[4]; varying float d[4];\n\
         void main() { gl_FragColor = vec4(m[0], c[1]) + a[1] + vec4(b[1], d[1]); }",
    );
    assert!(log.is_empty(), "{log}");
}

#[test]
fn structures_pass_between_the_stages() {
    // GLSL ES 3.00 allows structures as vertex outputs and fragment
    // inputs (without arrays or structures in them); members match by
    // name, a slot each.
    let p = program(
        "#version 300 es\nin vec4 p; struct L { vec3 n; float k; mat2 m; }; flat out struct F { int i; uint u; } f; out L l;\n\
         void main() { l = L(p.xyz, p.w, mat2(p)); f.i = 1; f.u = 2u; gl_Position = p; }",
        "#version 300 es\nprecision mediump float; struct L { vec3 n; float k; mat2 m; }; flat in struct F { int i; uint u; } f; in L l; out vec4 c;\n\
         void main() { c = vec4(l.n * l.k, l.m[1].y + float(f.i) + float(f.u)); }",
    );
    let names: std::vec::Vec<(&str, u32)> = p.linked.varyings.iter().map(|v| (v.name.as_str(), v.slots)).collect();
    assert_eq!(names, [("f", 2), ("l", 4)]);
    assert!(print(&p.fragment).contains("input 2.x"), "{}", print(&p.fragment));
    for (vs, fs, needle) in [
        (
            "#version 300 es\nstruct S { float a[2]; }; out S s; void main() { gl_Position = vec4(1.0); }",
            FS300,
            "basic types",
        ),
        (
            "#version 300 es\nstruct S { float a; }; out S s[2]; void main() { gl_Position = vec4(1.0); }",
            FS300,
            "arrays of structures",
        ),
        ("#version 300 es\nstruct S { int a; }; out S s; void main() { gl_Position = vec4(1.0); }", FS300, "'flat'"),
        (
            "#version 300 es\nstruct S { float a; }; in S s; void main() { gl_Position = vec4(1.0); }",
            FS300,
            "structures",
        ),
    ] {
        let o = Options::default();
        let c = compile(Stage::Vertex, &[vs], &o);
        assert!(c.shader.is_none() && c.log.contains(needle), "{vs}\n{}", c.log);
        let _ = fs;
    }
}

#[test]
fn linking_finds_names_by_lookup() {
    // Uniforms, varyings and transform feedback names are matched by
    // lookup, not each against every other (billions of steps for these).
    let n = 60_000;
    let decls: String = (0..n).map(|k| format!("uniform float u{k};\nout float v{k};\n")).collect();
    let vs = format!("#version 300 es\n{decls}void main() {{ gl_Position = vec4(1.0); }}");
    let ins: String = (0..n).map(|k| format!("in float v{k};\n")).collect();
    let fs = format!("#version 300 es\nprecision mediump float;\n{ins}out vec4 c; void main() {{ c = vec4(1.0); }}");
    let o = Options::default();
    let v = compile(Stage::Vertex, &[&vs], &o).shader.expect("vs");
    let f = compile(Stage::Fragment, &[&fs], &o).shader.expect("fs");
    let feedback: std::vec::Vec<String> = (0..n).map(|k| format!("v{k}")).collect();
    let bindings = Bindings { attributes: std::vec::Vec::new(), feedback, feedback_separate: false };
    let t = std::time::Instant::now();
    let r = link(&v, &f, &bindings, &o.limits);
    assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
    assert!(r.log.contains("too many varyings") && r.log.contains("too many interleaved"), "{}", r.log);
}

#[test]
fn feedback_finds_members_by_lookup() {
    // Transform feedback names members of structure outputs ("s.m"), each
    // found in a map of the structure's members, not compared with each of
    // them in turn (billions of string comparisons for these).
    let n = 30_000;
    let members: String = (0..n).map(|k| format!("float m{k};\n")).collect();
    let vs =
        format!("#version 300 es\nstruct S {{\n{members}}};\nout S s;\nvoid main() {{ gl_Position = vec4(1.0); }}");
    let fs = format!(
        "#version 300 es\nprecision mediump float;\nstruct S {{\n{members}}};\nin S s;\nout vec4 c; void main() {{ c = vec4(1.0); }}"
    );
    let o = Options::default();
    let v = compile(Stage::Vertex, &[&vs], &o);
    let f = compile(Stage::Fragment, &[&fs], &o);
    assert!(v.log.is_empty() && f.log.is_empty(), "{}{}", v.log, f.log);
    let feedback = std::vec![format!("s.m{}", n - 1); 120_000];
    let bindings = Bindings { attributes: std::vec::Vec::new(), feedback, feedback_separate: false };
    let t = std::time::Instant::now();
    let r = link(&v.shader.expect("vs"), &f.shader.expect("fs"), &bindings, &o.limits);
    assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
    assert!(r.log.contains("too many varyings") && r.log.contains("too many interleaved"), "{}", r.log);
}

#[test]
fn dominance_is_checked_in_linear_time() {
    // A value from the top used many times after a long run of ifs, each
    // one's block below the last in the dominator tree: the verifier
    // walked up the tree from every use (two billion steps here, where
    // optimised code only gets to tens of millions, but quadratically).
    let n = 25_000;
    let mut fs = String::from("precision mediump float; uniform vec2 v;\nvoid main() {\n");
    fs.push_str("float b = v.y; float c = 0.0; float acc = 1.0;\n");
    for k in 0..n {
        fs.push_str(&format!("if (v.x > {k}.0) c += 1.0;\n"));
    }
    for _ in 0..3 * n {
        fs.push_str("acc *= b;\n");
    }
    fs.push_str("gl_FragColor = vec4(acc, c, 0.0, 1.0);\n}\n");
    let o = Options::default();
    let v = compile(Stage::Vertex, &["void main() { gl_Position = vec4(0.0); }"], &o).shader.expect("vs");
    let f = compile(Stage::Fragment, &[&fs], &o).shader.expect("fs");
    let linked = crate::link::link(&v, &f, &Bindings::default(), &o.limits).linked.expect("linked");
    let code = crate::ir::build::build(&f, &linked.fragment, &linked).expect("built");
    let t = std::time::Instant::now();
    crate::ir::verify::verify(&code).expect("valid");
    assert!(t.elapsed().as_secs_f64() < 1.0, "{:?}", t.elapsed());
}
