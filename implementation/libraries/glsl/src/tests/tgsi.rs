// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The TGSI back end: every program translates into well-formed text
//! (balanced control flow, every register declared, nothing virglrenderer
//! cannot read). That the host runs it correctly is tested in `gl`,
//! against virglrenderer itself.

use std::format;
use std::string::String;
use std::vec::Vec;

use super::ir::program;
use crate::tgsi::{Options, Shader, translate};

/// Translates both stages of a program, checking each.
fn both(vs: &str, fs: &str) -> (Shader, Shader) {
    let p = program(vs, fs);
    let v = translate(&p.vertex, &p.linked, &Options::default());
    let f = translate(&p.fragment, &p.linked, &Options::default());
    check(&v.text);
    check(&f.text);
    (v, f)
}

/// The number in `FILE[n]` at the start of `s`.
fn index_after(s: &str, file: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let pat = format!("{file}[");
    let mut rest = s;
    while let Some(p) = rest.find(&pat) {
        // Not part of a longer name ("SVIEW[" contains no "IN[", but
        // "ADDR[0].x+" follows "CONST[").
        let before = rest[..p].chars().last();
        rest = &rest[p + pat.len()..];
        if before.is_some_and(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse() {
            out.push(n);
        }
    }
    out
}

/// The `n` of a declaration `DCL FILE[0..n]`.
fn declared_range(text: &str, file: &str) -> Option<u32> {
    let pat = format!("DCL {file}[0..");
    let p = text.find(&pat)?;
    let digits: String = text[p + pat.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn check(text: &str) {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    assert!(lines[0] == "VERT" || lines[0] == "FRAG", "{text}");
    assert_eq!(lines.last(), Some(&"END"), "{text}");
    let mut ifs = 0i32;
    let mut loops = 0i32;
    let mut seen_code = false;
    for l in &lines[1..] {
        let word = l.split([' ', '[']).next().unwrap_or("");
        match word {
            "DCL" | "IMM" | "PROPERTY" => assert!(!seen_code, "declaration after code:\n{text}"),
            _ => seen_code = true,
        }
        match word {
            "UIF" | "IF" => ifs += 1,
            "ENDIF" => ifs -= 1,
            "BGNLOOP" => loops += 1,
            "ENDLOOP" => loops -= 1,
            "CONT" => panic!("virglrenderer has no CONT:\n{text}"),
            _ => {}
        }
        assert!(ifs >= 0 && loops >= 0, "unbalanced control flow:\n{text}");
        // Sources always have four-component swizzles.
        if !matches!(word, "DCL" | "IMM" | "PROPERTY") {
            for operand in l.split(", ").skip(1) {
                let operand = operand.trim_start_matches(['-', '|']).trim_end_matches('|');
                if let Some(dot) = operand.rfind("].") {
                    let swz = &operand[dot + 2..];
                    let target_like = swz.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                    assert!(
                        swz.len() == 4 || target_like || operand.starts_with("IMM") && swz == "xyz",
                        "short swizzle in `{l}`:\n{text}"
                    );
                }
            }
        }
    }
    assert_eq!((ifs, loops), (0, 0), "unbalanced control flow:\n{text}");
    let temps = declared_range(text, "TEMP").map_or(0, |n| n + 1);
    for t in index_after(text, "TEMP") {
        assert!(t < temps, "TEMP[{t}] not declared (have {temps}):\n{text}");
    }
    let imms = lines.iter().filter(|l| l.starts_with("IMM[")).count() as u32;
    for i in index_after(text, "IMM") {
        assert!(i < imms, "IMM[{i}] not declared:\n{text}");
    }
    // An IF names its ELSE or ENDIF, an ELSE its ENDIF: where TGSI's
    // interpreter goes on when no invocation takes the branch.
    let code: Vec<&str> = lines[1..]
        .iter()
        .copied()
        .filter(|l| !matches!(l.split([' ', '[']).next(), Some("DCL" | "IMM" | "PROPERTY")))
        .collect();
    let label = |l: &str| l.rsplit_once(" :").and_then(|(_, n)| n.parse::<usize>().ok());
    let mut open = Vec::new();
    for (i, l) in code.iter().enumerate() {
        match l.split(' ').next().unwrap_or("") {
            "UIF" | "IF" => open.push(i),
            w @ ("ELSE" | "ENDIF") => {
                let at = open.pop().expect("balanced");
                assert_eq!(label(code[at]), Some(i), "`{}` does not go on at `{l}`:\n{text}", code[at]);
                if w == "ELSE" {
                    open.push(i);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn a_transform_and_a_color() {
    let (v, f) = both(
        "attribute vec4 p; attribute vec2 uv; uniform mat4 mvp; varying vec2 t;\n\
         void main() { t = uv; gl_Position = mvp * p; }",
        "precision mediump float; varying vec2 t; uniform vec4 tint;\n\
         void main() { gl_FragColor = vec4(t, 0.0, 1.0) * tint; }",
    );
    assert!(v.text.contains("DCL OUT[0], POSITION"), "{}", v.text);
    assert!(v.text.contains("GENERIC[0]"), "{}", v.text);
    assert!(f.text.contains("PROPERTY FS_COLOR0_WRITES_ALL_CBUFS 1"), "{}", f.text);
    assert!(f.text.contains("DCL IN[0], GENERIC[0], PERSPECTIVE"), "{}", f.text);
    assert!(f.text.contains("DCL OUT[0], COLOR[0]"), "{}", f.text);
    assert_eq!(v.position, Some(0));
    assert_eq!(v.varyings, std::vec![1]);
}

#[test]
fn loops_continue_through_a_flag() {
    let (v, _) = both(
        "#version 300 es\nuniform int n; out float r;\n\
         void main() {\n\
           float acc = 0.0;\n\
           for (int i = 0; i < 10; i++) {\n\
             if (i == n) continue;\n\
             if (i > 7) { if (acc > 3.0) continue; acc += 0.5; }\n\
             for (int j = 0; j < i; ++j) { if (j > 3) break; acc += float(j); }\n\
             acc *= 1.5;\n\
           }\n\
           r = acc; gl_Position = vec4(acc);\n\
         }",
        "#version 300 es\nprecision mediump float; in float r; out vec4 c; void main() { c = vec4(r); }",
    );
    assert!(v.text.contains("BGNLOOP"), "{}", v.text);
    assert!(v.text.contains("BRK"), "{}", v.text);
}

#[test]
fn textures_of_every_kind() {
    let (_, f) = both(
        "#version 300 es\nin vec4 p; out vec4 q; void main() { q = p; gl_Position = p; }",
        "#version 300 es\nprecision highp float;\n\
         uniform sampler2D a; uniform highp sampler3D b; uniform samplerCube c; uniform highp sampler2DArray d;\n\
         uniform highp sampler2DShadow e; uniform highp samplerCubeShadow g; uniform highp sampler2DArrayShadow h;\n\
         uniform highp isampler2D i; uniform highp usampler2D u;\n\
         in vec4 q; out vec4 o;\n\
         void main() {\n\
           vec4 s = texture(a, q.xy) + texture(a, q.xy, 1.0) + textureLod(a, q.xy, 2.0)\n\
             + textureOffset(a, q.xy, ivec2(1, -2)) + textureGrad(a, q.xy, q.zw, q.wz)\n\
             + texelFetch(a, ivec2(q.xy), 0) + textureProj(a, q.xyz)\n\
             + texture(b, q.xyz) + texture(c, q.xyz) + textureLod(c, q.xyz, 1.0) + texture(d, q.xyz);\n\
           float z = texture(e, q.xyz) + texture(g, q) + texture(h, q) + textureLod(e, q.xyz, 0.0);\n\
           ivec2 sz = textureSize(a, 0) + textureSize(c, 1);\n\
           ivec3 sz3 = textureSize(b, 0) + textureSize(d, 0);\n\
           o = s + vec4(z) + vec4(sz.xyxy) + vec4(sz3, 0.0)\n\
             + vec4(texture(i, q.xy)) + vec4(texelFetch(u, ivec2(q.xy), 0));\n\
         }",
    );
    for needle in [
        "TEX ",
        "TXB ",
        "TXL ",
        "TXD ",
        "TXF ",
        "TXQ ",
        "SHADOW2D",
        "SHADOWCUBE",
        "SHADOW2D_ARRAY",
        "2D_ARRAY",
        "CUBE",
        "3D",
        "SINT",
        "UINT",
    ] {
        assert!(f.text.contains(needle), "no {needle}:\n{}", f.text);
    }
    assert_eq!(f.samplers.len(), 9);
}

#[test]
fn indexing_uniforms_blocks_and_samplers() {
    let (v, f) = both(
        "#version 300 es\nuniform vec4 table[8]; uniform int k; in vec4 p;\n\
         layout(std140) uniform Lights { vec4 color[4]; mat3 m; float scale[3]; };\n\
         out vec4 q;\n\
         void main() { q = table[k] + color[k & 3] + vec4(m[k % 3], scale[k % 3]); gl_Position = p; }",
        "#version 300 es\nprecision mediump float; in vec4 q; out vec4 c; void main() { c = q; }",
    );
    assert!(v.text.contains("UARL ADDR[0].x"), "{}", v.text);
    assert!(v.text.contains("CONST[1][ADDR[0].x+"), "{}", v.text);
    assert_eq!(v.blocks, std::vec![0]);
    assert!(f.text.contains("DCL OUT[0], COLOR[0]"), "{}", f.text);
    // GLSL ES 1.00 indexes sampler arrays with loop indices.
    let (_, f) = both(
        "attribute vec4 p; void main() { gl_Position = p; }",
        "precision mediump float; uniform sampler2D s[3];\n\
         void main() { gl_FragColor = vec4(0.0);\n\
           for (int i = 0; i < 3; i++) gl_FragColor += texture2D(s[i], vec2(0.5)); }",
    );
    assert!(f.text.contains("SAMP[2]"), "{}", f.text);
}

#[test]
fn functions_the_host_lacks() {
    let (_, f) = both(
        "#version 300 es\nin vec4 p; out vec4 q; void main() { q = p; gl_Position = p; }",
        "#version 300 es\nprecision highp float; in vec4 q; out vec4 o;\n\
         void main() {\n\
           vec4 a = asin(q) + acos(q) + atan(q) + atan(q, q.yzwx) + tan(q);\n\
           vec4 b = sinh(q) + cosh(q) + tanh(q) + asinh(q) + acosh(q + 2.0) + atanh(q * 0.5);\n\
           bvec4 n = isnan(q); bvec4 i = isinf(q);\n\
           uint h = packHalf2x16(q.xy); vec2 back = unpackHalf2x16(h);\n\
           o = a + b + vec4(n) + vec4(i) + vec4(back, exp(q.x), log(q.y));\n\
         }",
    );
    assert!(f.text.contains("EX2"), "{}", f.text);
    assert!(f.text.contains("LG2"), "{}", f.text);
}

#[test]
fn integers_and_flat_varyings() {
    let (v, f) = both(
        "#version 300 es\nin ivec4 p; flat out ivec2 m; flat out uint w;\n\
         void main() { m = p.xy / (p.zw | 1) + (p.xy % 7) - (p.xy << 2) + (p.xy >> 1);\n\
           w = uint(p.x) * 3u + (uint(p.y) >> 3u); gl_Position = vec4(p); }",
        "#version 300 es\nprecision highp float; flat in ivec2 m; flat in uint w; layout(location = 1) out ivec4 c;\n\
         void main() { c = ivec4(m, int(w), abs(m.x) * sign(m.y)); }",
    );
    assert!(f.text.contains("CONSTANT"), "{}", f.text);
    assert!(f.text.contains("COLOR[1]"), "{}", f.text);
    assert!(v.text.contains("IDIV"), "{}", v.text);
}

#[test]
fn fragment_built_ins() {
    let (v, f) = both(
        "#version 300 es\nin vec4 p; void main() { gl_PointSize = 4.0; gl_Position = p * float(gl_VertexID + gl_InstanceID); }",
        "#version 300 es\nprecision highp float; out vec4 c;\n\
         void main() { c = vec4(gl_FragCoord.xy, gl_PointCoord) * (gl_FrontFacing ? 1.0 : 0.5);\n\
           if (c.x > 100.0) discard; gl_FragDepth = 0.25; }",
    );
    assert!(v.text.contains("VERTEXID"), "{}", v.text);
    assert!(v.text.contains("INSTANCEID"), "{}", v.text);
    assert!(v.text.contains("PSIZE"), "{}", v.text);
    assert!(v.point_size.is_some());
    for needle in ["POSITION, LINEAR", "FACE", "PCOORD", "KILL", "OUT[1].z"] {
        assert!(f.text.contains(needle), "no {needle}:\n{}", f.text);
    }
}

#[test]
fn derivatives_and_selects() {
    let (_, f) = both(
        "#version 300 es\nin vec4 p; out vec2 t; void main() { t = p.xy; gl_Position = p; }",
        "#version 300 es\nprecision highp float; in vec2 t; out vec4 c;\n\
         void main() { vec2 d = dFdx(t) + dFdy(t) + fwidth(t); c = vec4(d, t.x > 0.5 ? t.y : -t.y, 1.0); }",
    );
    assert!(f.text.contains("DDX") && f.text.contains("DDY"), "{}", f.text);
    assert!(f.text.contains("UCMP"), "{}", f.text);
}

#[test]
fn constants_and_uniforms_stay_inside_the_program() {
    // An index constant only once inlined, and far outside the array:
    // clamped, so nothing past the program's uniforms is declared or read.
    let (v, _) = both(
        "#version 300 es\nuniform vec4 u[4]; uniform int k;\nfloat p(int i) { return u[i][k]; }\n\
         void main() { gl_Position = vec4(p(7) + p(-1) + p(100000)); }",
        "#version 300 es\nprecision mediump float; out vec4 c; void main() { c = vec4(1.0); }",
    );
    // Four slots of `u`, one of `k`, and the zero slot after them.
    assert!(declared_range(&v.text, "CONST").is_some_and(|n| n <= 5), "{}", v.text);
    for c in index_after(&v.text, "CONST") {
        assert!(c <= 5, "CONST[{c}]:\n{}", v.text);
    }
}

#[test]
fn sampler_arrays_declare_only_their_own_samplers() {
    // A loop over a sampler array next to a cube map: the lookups choose
    // among the array's samplers only, and each sampler is declared with
    // its own type.
    let (_, f) = both(
        "attribute vec4 p; varying vec2 uv; void main() { uv = p.xy; gl_Position = p; }",
        "precision mediump float; uniform sampler2D s[2]; uniform samplerCube c; varying vec2 uv;\n\
         void main() { vec4 a = textureCube(c, vec3(uv, 1.0));\n\
           for (int i = 0; i < 2; i++) a += texture2D(s[i], uv);\n\
           gl_FragColor = a; }",
    );
    let decls: Vec<&str> = f.text.lines().filter(|l| l.starts_with("DCL SVIEW")).collect();
    assert_eq!(
        decls,
        ["DCL SVIEW[0], 2D, FLOAT", "DCL SVIEW[1], 2D, FLOAT", "DCL SVIEW[2], CUBE, FLOAT"],
        "{}",
        f.text
    );
    for line in f.text.lines().filter(|l| l.contains("SAMP[2]") && !l.starts_with("DCL")) {
        assert!(line.contains("CUBE"), "{line}");
    }
}

#[test]
fn phi_copies_in_long_cycles_are_quick() {
    // A loop rotating five thousand values: its back edge copies each into
    // the next, one cycle (the copy that can be made first always the
    // last of those left). Ordering those copies must not take cubic time.
    let n = 5000;
    let decls: String = (0..n).map(|k| std::format!("float a{k} = p.x * {k}.0;\n")).collect();
    let rotate: String = (1..n).rev().map(|k| std::format!("a{k} = a{};\n", k - 1)).collect();
    let sum: String = (0..n).map(|k| std::format!("s += a{k};\n")).collect();
    let vs = std::format!(
        "#version 300 es\nin vec4 p; uniform int m;\nvoid main() {{\n{decls}for (int i = 0; i < m; i++) {{ float t = a{};\n{rotate}a0 = t; }}\n\
         float s = 0.0;\n{sum}gl_Position = vec4(s);\n}}",
        n - 1
    );
    let t = std::time::Instant::now();
    both(&vs, "#version 300 es\nprecision mediump float; out vec4 c; void main() { c = vec4(1.0); }");
    let p =
        super::ir::program(&vs, "#version 300 es\nprecision mediump float; out vec4 c; void main() { c = vec4(1.0); }");
    crate::interp::compile(&p.vertex);
    assert!(t.elapsed().as_secs_f64() < 10.0, "{:?}", t.elapsed());
}

#[test]
fn loops_left_by_many_jumps_are_linear() {
    // A loop that many `break`s or `continue`s leave: finding its blocks'
    // dominators, and each jump's place among its target's predecessors
    // (for its φ copies), took time quadratic in them; and the TGSI after
    // each `continue` was guarded inside the guard after the one before,
    // nesting the text as deep as there were `continue`s.
    for (jump, n) in [("break", 60_000), ("continue", 5_000)] {
        let ifs = std::format!("if (v.x > 0.5) {jump};\n").repeat(n);
        let fs = std::format!(
            "precision mediump float; uniform vec2 v;\nvoid main() {{\nfloat c = 0.0;\n\
             for (int i = 0; i < 4; i++) {{\n{ifs}c += 1.0;\n}}\ngl_FragColor = vec4(c);\n}}\n"
        );
        let (elapsed, text) = super::on_small_stack(move || {
            let t = std::time::Instant::now();
            let p = program("void main() { gl_Position = vec4(0.0); }", &fs);
            crate::interp::compile(&p.fragment);
            let text = translate(&p.fragment, &p.linked, &Options::default()).text;
            (t.elapsed().as_secs_f64(), text)
        });
        check(&text);
        assert!(elapsed < 5.0, "{jump}: {elapsed} s");
        let mut depth = 0usize;
        let mut deepest = 0;
        for l in text.lines() {
            match l.trim().split([' ', '[']).next().unwrap_or("") {
                "UIF" | "IF" | "BGNLOOP" => depth += 1,
                "ENDIF" | "ENDLOOP" => depth = depth.saturating_sub(1),
                _ => {}
            }
            deepest = deepest.max(depth);
        }
        assert!(deepest <= 4, "{jump}: nested {deepest} deep");
        assert!(text.len() < 400 * n, "{jump}: {} bytes", text.len());
    }
}
