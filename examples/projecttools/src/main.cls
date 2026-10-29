import * as fs from "std/fs";
import * as mcp from "std/mcp";
import { Stats, summarize } from "./stats";

interface FileInfo { path: string; size: number; isDir: boolean }

@tool({ description: "List files in a project directory", readOnly: true })
async function listFiles(dir: string, recursive?: boolean): Promise<FileInfo[]> {
  const entries = await fs.readDir(dir, recursive === true);
  return entries.map((e) => ({ path: e.path, size: e.size, isDir: e.isDir }));
}

@tool({ description: "Read a UTF-8 text file", readOnly: true })
async function readText(path: string): Promise<string> {
  return await fs.readFile(path);
}

@tool({ description: "Write a text file inside the project", destructive: true })
async function writeText(path: string, content: string): Promise<mcp.ToolResult> {
  await fs.writeFile(path, content);
  return mcp.text(`wrote ${content.length} chars to ${path}`);
}

@tool({ description: "Statistics for a list of numbers" })
function stats(values: number[]): Stats {
  return summarize(values);
}

@tool({ description: "Slow job reporting progress" })
async function slow(steps: number): Promise<string> {
  for (let i = 1; i <= steps; i++) {
    mcp.progress(i, steps, `step ${i}`);
    mcp.log("info", `finished step ${i}`);
  }
  return "done";
}

@tool({ description: "Always fails" })
function boom(reason: string): string {
  throw Error(`failed: ${reason}`);
}

@tool({ description: "Busy loop forever (hits the time limit)" })
function spin(): number {
  let n = 0;
  while (true) { n += 1; }
}

@tool({ description: "Pick a mode" })
function mode(kind: "fast" | "slow", tags?: string[]): string {
  return `${kind}:${(tags ?? []).join("+")}`;
}

@resource("project://readme", { description: "Project notes", mimeType: "text/plain" })
async function readme(): Promise<string> {
  return await fs.readFile("data/notes.txt");
}

@resource("project://file/{name}", { description: "A data file", mimeType: "text/plain" })
async function dataFile(name: string): Promise<string> {
  return await fs.readFile(`data/${name}`);
}

@prompt({ description: "Summarize a file", params: { path: "File to summarize" } })
function summarizePrompt(path: string, style?: string): string {
  return `Summarize ${path} in a ${style ?? "neutral"} style.`;
}

test "stats" {
  const s = summarize([1, 2, 3, 4]);
  assertEq(s.mean, 2.5);
  assertEq(s.max, 4);
}
