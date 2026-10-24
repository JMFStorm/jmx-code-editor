#!/usr/bin/env python3
"""Generate sandbox/stress: a large, deterministic workspace to stress-test JDC.

  web/      JS/TS/Vue monorepo: ~20k source files in 40 packages, 2k spec files (~20k tests),
            .d.ts types and @typedefs, a fake node_modules (3k folders), dist/ build output,
            and big files: big.js (200k lines), bundle.min.js (3 MB, one line),
            data.json (20 MB), server.log (every line matches a common word).
  dotnet/   4 C# projects x 400 files (XML docs, interfaces + implementations) and a test
            project with 2k xUnit tests (not built by the stress runs).
  stress.jdcws opens both.

Usage: tools/make-stress-sandbox.py [--scale 1.0] [--out sandbox/stress]
"""

import argparse
import json
import os
import random
import shutil

WORDS = ("alpha beta gamma delta value item order user account price stock total count "
         "result record entry status level index cache queue batch event filter mapper "
         "parser render update create remove fetch format compute").split()


def w(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def camel(rng, n=2):
    parts = [rng.choice(WORDS) for _ in range(n)]
    return parts[0] + "".join(p.capitalize() for p in parts[1:])


def pascal(rng, n=2):
    return "".join(rng.choice(WORDS).capitalize() for _ in range(n))


def js_function(rng, name, uses):
    a, b = camel(rng, 1), camel(rng, 1) + "List"
    calls = "\n".join(f"  const r{i} = {u}({a}, {i});" for i, u in enumerate(uses))
    return f"""/**
 * {pascal(rng, 3)} for the {rng.choice(WORDS)} {rng.choice(WORDS)}.
 *
 * @param {{number}} {a} - The {rng.choice(WORDS)} to use.
 * @param {{string[]}} [{b}] - Optional {rng.choice(WORDS)} names.
 * @returns {{object}} The computed result.
 */
export function {name}({a}, {b} = []) {{
  // Compute the result for "{rng.choice(WORDS)}".
  const result = {{ {a}, size: {b}.length, label: `item ${{{a}}}` }};
{calls}
  if ({a} > {rng.randint(1, 999)}) {{
    return {{ ...result, flag: /^[a-z]+\\d*$/.test(String({a})) }};
  }}
  return result;
}}
"""


def make_web(root, rng, scale):
    packages = 40
    files_per_pkg = int(500 * scale)
    specs_per_pkg = int(50 * scale)
    w(f"{root}/package.json", json.dumps({"name": "stress-web", "private": True}, indent=2))
    w(f"{root}/.gitignore", "node_modules/\ndist/\n*.log.tmp\n")
    w(f"{root}/types/models.d.ts", "\n".join(
        f"/** A {rng.choice(WORDS)} record. */\nexport interface {pascal(rng)}{i} {{\n  id: number;\n"
        f"  name: string;\n  /** When it changed. */\n  updatedAt: string;\n}}\n" for i in range(200)))
    # Shared helpers used everywhere: references and Find in Files hit them a lot.
    w(f"{root}/packages/shared/src/format.js",
      js_function(rng, "formatValue", []) + js_function(rng, "computeTotal", []))
    for p in range(packages):
        pkg = f"{root}/packages/pkg{p:02d}"
        w(f"{pkg}/package.json", json.dumps({"name": f"pkg{p:02d}", "version": "1.0.0"}))
        for f in range(files_per_pkg):
            names = [f"{camel(rng)}{p}_{f}_{k}" for k in range(4)]
            body = "import { formatValue, computeTotal } from '../../shared/src/format.js';\n"
            body += "/** @typedef {import('../../../types/models').ValueItem0} ValueItem0 */\n\n"
            body += "".join(js_function(rng, n, ["formatValue", "computeTotal"]) for n in names)
            ext = ".ts" if f % 10 == 0 else ".js"
            w(f"{pkg}/src/mod{f:04d}{ext}", body)
        for f in range(int(files_per_pkg / 50)):
            w(f"{pkg}/src/components/Comp{f:03d}.vue", f"""<template>
  <div class="comp">{{{{ formatValue(total) }}}}</div>
</template>
<script setup>
import {{ computed }} from 'vue';
import {{ formatValue }} from '../../../shared/src/format.js';
const total = computed(() => {f});
</script>
""")
        for s in range(specs_per_pkg):
            tests = "\n".join(
                f"  it('{camel(rng, 3)} {t}', () => {{\n    expect(formatValue({t})).toBeTruthy();\n  }});"
                for t in range(10))
            w(f"{pkg}/tests/spec{s:03d}.spec.js",
              f"import {{ describe, it, expect }} from 'vitest';\n"
              f"import {{ formatValue }} from '../../shared/src/format.js';\n\n"
              f"describe('{pascal(rng)} {s}', () => {{\n{tests}\n}});\n")
        w(f"{root}/dist/pkg{p:02d}.js", "// build output\n" * 200)
    # A fake node_modules: lots of folders (watcher, ignore handling) and one typed package.
    for i in range(int(3000 * scale)):
        w(f"{root}/node_modules/dep{i:04d}/package.json", json.dumps({"name": f"dep{i:04d}"}))
    w(f"{root}/node_modules/vue/package.json", json.dumps({"name": "vue", "types": "index.d.ts"}))
    w(f"{root}/node_modules/vue/index.d.ts",
      "/** Takes a getter function and returns a readonly reactive ref. */\n"
      "export declare function computed<T>(getter: () => T): { readonly value: T };\n")
    # Big files.
    lines = []
    for i in range(int(200_000 * scale)):
        k = i % 8
        if k == 0:
            lines.append(f"/** Block {i}: {rng.choice(WORDS)} {rng.choice(WORDS)}. */")
        elif k == 1:
            lines.append(f"export function {camel(rng)}{i}(value, list = []) {{")
        elif k == 2:
            lines.append(f"  const text = 'result {i}' + \"{rng.choice(WORDS)}\" + `x ${{value}}`;")
        elif k == 3:
            lines.append(f"  if (/^v\\d+$/.test(text) && value > {i}) {{ return formatValue(value); }}")
        elif k == 4:
            lines.append(f"  // {' '.join(rng.choice(WORDS) for _ in range(8))}")
        elif k == 5:
            lines.append(f"  return list.map((x) => x * {i % 97}).filter(Boolean);")
        elif k == 6:
            lines.append("}")
        else:
            lines.append("")
    w(f"{root}/big/big.js", "\n".join(lines) + "\n")
    chunk = "function a(b,c){return b+c*2}var x=[1,2,3].map(function(v){return a(v,1)});"
    w(f"{root}/big/bundle.min.js", (chunk * (3_000_000 // len(chunk))) + "\n")
    rows = [{"id": i, "name": camel(rng, 3), "value": rng.random(), "tags": [rng.choice(WORDS)]}
            for i in range(int(200_000 * scale))]
    w(f"{root}/big/data.json", json.dumps(rows, indent=1))
    w(f"{root}/big/server.log", "".join(
        f"2026-10-24T12:{i // 3600 % 60:02d}:{i % 60:02d} INFO request handled result=ok id={i}\n"
        for i in range(int(60_000 * scale))))


def make_dotnet(root, rng, scale):
    w(f"{root}/global.json", json.dumps({"sdk": {"version": "10.0.100", "rollForward": "latestMajor"}}))
    w(f"{root}/.gitignore", "bin/\nobj/\n")
    files = int(400 * scale)
    for p in range(4):
        name = f"Stress.Module{p}"
        proj = f"{root}/src/{name}"
        w(f"{proj}/{name}.csproj", """<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
  </PropertyGroup>
</Project>
""")
        for f in range(files):
            cls = f"{pascal(rng)}Service{f}"
            methods = "\n".join(f"""    /// <summary>Computes the {rng.choice(WORDS)} for <paramref name="id"/>.</summary>
    /// <param name="id">The record id.</param>
    /// <returns>The {rng.choice(WORDS)} value.</returns>
    public async Task<int> Compute{m}Async(int id, CancellationToken ct = default)
    {{
        var items = Enumerable.Range(0, id).Where(x => x % {m + 2} == 0).ToList();
        await Task.Delay(0, ct);
        return items.Count + Helper.Total(id);
    }}
""" for m in range(5))
            w(f"{proj}/Services/{cls}.cs", f"""namespace {name}.Services;

/// <summary>The {rng.choice(WORDS)} service number {f}.</summary>
public interface I{cls}
{{
    /// <summary>Computes the first value.</summary>
    Task<int> Compute0Async(int id, CancellationToken ct = default);
}}

/// <summary>Implements <see cref="I{cls}"/>.</summary>
public sealed class {cls} : I{cls}
{{
{methods}}}
""")
        w(f"{proj}/Helper.cs", f"""namespace {name};

public static class Helper
{{
    /// <summary>Totals a value.</summary>
    public static int Total(int value) => value * 2;
}}
""")
    tests = f"{root}/tests/Stress.Tests"
    w(f"{tests}/Stress.Tests.csproj", """<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <IsPackable>false</IsPackable>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="xunit" Version="2.9.3" />
  </ItemGroup>
</Project>
""")
    for f in range(int(100 * scale)):
        body = "\n".join(f"""    [Fact]
    public void {pascal(rng, 3)}_{t}() => Assert.Equal({t}, {t});
""" for t in range(20))
        w(f"{tests}/Tests{f:03d}.cs", f"namespace Stress.Tests;\n\npublic class Tests{f:03d}\n{{\n{body}}}\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scale", type=float, default=1.0)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "sandbox", "stress"))
    args = ap.parse_args()
    out = os.path.abspath(args.out)
    if os.path.exists(out):
        shutil.rmtree(out)
    rng = random.Random(42)
    make_web(f"{out}/web", rng, args.scale)
    make_dotnet(f"{out}/dotnet", rng, args.scale)
    w(f"{out}/stress.jdcws", json.dumps({
        "version": 1, "name": "stress", "folders": ["web", "dotnet"], "respect_gitignore": True,
    }, indent=2))
    total = sum(len(fs) for _, _, fs in os.walk(out))
    size = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(out) for f in fs)
    print(f"wrote {out}: {total} files, {size / 1e6:.0f} MB")


if __name__ == "__main__":
    main()
