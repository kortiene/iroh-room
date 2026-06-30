import {
	BorderedLoader,
	DynamicBorder,
	type ExecResult,
	type ExtensionAPI,
	type ExtensionContext,
	getSelectListTheme,
} from "@earendil-works/pi-coding-agent";
import {
	type AutocompleteItem,
	type AutocompleteProvider,
	type AutocompleteSuggestions,
	Container,
	fuzzyFilter,
	type SelectItem,
	SelectList,
	Text,
	truncateToWidth,
	visibleWidth,
} from "@earendil-works/pi-tui";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const WIDGET_ID = "adw-cockpit";
const HELP_WIDGET_ID = "adw-cockpit-help";
const STATUS_ID = "adw-cockpit";
const COMMAND_OUTPUT_LIMIT = 1200;

/**
 * The configurable agent-phase chain (mirrors adw_sdlc AGENT_PHASES /
 * DEFAULT_PHASES). The cockpit renders the full pipeline as
 * setup → <agent chain> → merge; the finalize/ci-fix/report kernel wrappers
 * are never recorded in completed_phases, so they are intentionally not shown
 * as tracked phases.
 */
const DEFAULT_AGENT_PHASES = [
	"classify",
	"plan",
	"implement",
	"tests",
	"resolve",
	"e2e",
	"review",
	"patch",
	"document",
] as const;

type ThemeApi = ExtensionContext["ui"]["theme"];

interface CommandSummary {
	label: string;
	command: string;
	code: number;
	killed: boolean;
	durationMs: number;
	timestamp: number;
	stdout: string;
	stderr: string;
}

let lastCommandSummary: CommandSummary | null = null;

interface AdwConfigSummary {
	projectName: string;
	providerSummary: string;
	testGate: string;
	finalizeGateCount: number;
	/** The resolved agent-phase chain (config `phases`, else the built-in default). */
	agentPhases: string[];
	/** Where prompt templates resolve from (`prompts.defaultRoot`, e.g. `.adw/prompts`). */
	promptsRoot: string;
	/** Whether a generated prompt-pack profile (`.adw/pack.profile.json`) is present. */
	hasPack: boolean;
}

interface LatestRunSummary {
	adwId: string;
	issueNumber: string | null;
	runner: string | null;
	branchName: string | null;
	completed: string[];
	prUrl: string | null;
	totalCostUsd: number | null;
	mtimeMs: number;
}

interface GitSummary {
	branch: string;
	dirtyCount: number;
}

function parseJsonObject(path: string): Record<string, unknown> | null {
	try {
		const parsed = JSON.parse(readFileSync(path, "utf8"));
		return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
	} catch {
		return null;
	}
}

function recordAt(value: unknown, key: string): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value)) return {};
	const nested = (value as Record<string, unknown>)[key];
	return nested && typeof nested === "object" && !Array.isArray(nested) ? (nested as Record<string, unknown>) : {};
}

function stringAt(value: unknown, key: string, fallback = "unknown"): string {
	if (!value || typeof value !== "object" || Array.isArray(value)) return fallback;
	const raw = (value as Record<string, unknown>)[key];
	return typeof raw === "string" && raw.length > 0 ? raw : fallback;
}

function readConfigSummary(cwd: string): AdwConfigSummary | null {
	const path = join(cwd, ".adw", "config.json");
	if (!existsSync(path)) return null;

	const hasPack = existsSync(join(cwd, ".adw", "pack.profile.json"));
	const config = parseJsonObject(path);
	if (!config) return {
		projectName: "invalid .adw/config.json",
		providerSummary: "unknown/unknown/unknown",
		testGate: "unknown",
		finalizeGateCount: 0,
		agentPhases: [...DEFAULT_AGENT_PHASES],
		promptsRoot: "unknown",
		hasPack,
	};

	const project = recordAt(config, "project");
	const prompts = recordAt(config, "prompts");
	const providers = recordAt(config, "providers");
	const workItems = recordAt(providers, "workItems");
	const vcs = recordAt(providers, "vcs");
	const changeRequests = recordAt(providers, "changeRequests");
	const commands = recordAt(config, "commands");
	const finalizeGates = commands["defaultFinalizeGates"];
	const defaultTestCommand = commands["defaultTestCommand"];
	// The run's agent-phase chain: config `phases` when present and non-empty,
	// else the built-in default (mirrors adw_sdlc parsePhases precedence).
	const phasesRaw = config["phases"];
	const agentPhases =
		Array.isArray(phasesRaw) && phasesRaw.some((p) => typeof p === "string")
			? phasesRaw.filter((p): p is string => typeof p === "string")
			: [...DEFAULT_AGENT_PHASES];

	return {
		projectName: stringAt(project, "name", "unknown project"),
		providerSummary: `${stringAt(workItems, "type")}/${stringAt(vcs, "type")}/${stringAt(changeRequests, "type")}`,
		testGate: typeof defaultTestCommand === "string" && defaultTestCommand.trim() ? defaultTestCommand.trim() : "none",
		finalizeGateCount: Array.isArray(finalizeGates) ? finalizeGates.length : 0,
		agentPhases,
		promptsRoot: stringAt(prompts, "defaultRoot", "unknown"),
		hasPack,
	};
}

function gitOutput(cwd: string, args: string[]): string | null {
	try {
		return execFileSync("git", args, {
			cwd,
			encoding: "utf8",
			stdio: ["ignore", "pipe", "ignore"],
			timeout: 1500,
		}).trim();
	} catch {
		return null;
	}
}

function readGitSummary(cwd: string): GitSummary {
	const branch = gitOutput(cwd, ["branch", "--show-current"]) || "detached/unknown";
	const status = gitOutput(cwd, ["status", "--short"]);
	const dirtyCount = status ? status.split("\n").filter((line) => line.trim().length > 0).length : 0;
	return { branch, dirtyCount };
}

/** All ADW run workspaces under agents/, newest first (by state.json mtime). */
function listRuns(cwd: string): LatestRunSummary[] {
	const agentsDir = join(cwd, "agents");
	if (!existsSync(agentsDir)) return [];

	const runs: LatestRunSummary[] = [];
	for (const entry of readdirSync(agentsDir, { withFileTypes: true })) {
		if (!entry.isDirectory()) continue;
		const statePath = join(agentsDir, entry.name, "state.json");
		if (!existsSync(statePath)) continue;

		const state = parseJsonObject(statePath);
		if (!state) continue;

		let mtimeMs = 0;
		try {
			mtimeMs = statSync(statePath).mtimeMs;
		} catch {
			mtimeMs = 0;
		}

		const completedRaw = state["completed_phases"];
		const completed = Array.isArray(completedRaw)
			? completedRaw.filter((phase): phase is string => typeof phase === "string")
			: [];

		runs.push({
			adwId: typeof state["adw_id"] === "string" ? state["adw_id"] : entry.name,
			issueNumber: typeof state["issue_number"] === "string" ? state["issue_number"] : null,
			runner: typeof state["runner"] === "string" ? state["runner"] : null,
			branchName: typeof state["branch_name"] === "string" ? state["branch_name"] : null,
			completed,
			prUrl: typeof state["pr_url"] === "string" ? state["pr_url"] : null,
			totalCostUsd: typeof state["total_cost_usd"] === "number" ? state["total_cost_usd"] : null,
			mtimeMs,
		});
	}
	runs.sort((a, b) => b.mtimeMs - a.mtimeMs);
	return runs;
}

function latestRun(cwd: string): LatestRunSummary | null {
	return listRuns(cwd)[0] ?? null;
}

/**
 * A copy-pasteable resume command for a run, or null when the run has no
 * recorded issue number (the CLI's positional work-item id is required, and a
 * resumed run refuses a mismatched number). The extension never executes this;
 * it only inserts the text into the editor for the user to run deliberately.
 */
function resumeCommandFor(run: LatestRunSummary): string | null {
	if (!run.issueNumber) return null;
	return `cd adw_sdlc && npm run issue -- ${run.issueNumber} --resume --adw-id ${run.adwId}`;
}

type PrStatus = "merged" | "open" | "none";

/**
 * PR lifecycle for a run, derived from local state alone. A recorded `merge`
 * phase is the kernel's own definition of "already merged" — adw_sdlc's
 * orchestrator uses `state.isDone('merge')` as its post-merge resume guard — so
 * a completed run reads as "merged" even though pr_url stays populated (the
 * merged PR's URL is still valid). A pr_url with no merge phase is a genuinely
 * open PR; no pr_url means none was opened. The dashboard render path is
 * offline, so this never hits the network — it trusts the same signal the
 * kernel persists.
 */
function prStatus(run: LatestRunSummary): PrStatus {
	if (!run.prUrl) return "none";
	return run.completed.includes("merge") ? "merged" : "open";
}

/** Status → display label + theme colour (merged = done, open = not yet merged). */
function prStatusStyle(status: PrStatus): { label: string; color: "success" | "warning" | "dim" } {
	if (status === "merged") return { label: "merged", color: "success" };
	if (status === "open") return { label: "open", color: "warning" };
	return { label: "none", color: "dim" };
}

/** Pre-styled `pr` value for the Latest Run panel. */
function prCell(theme: ThemeApi, run: LatestRunSummary): string {
	const style = prStatusStyle(prStatus(run));
	return theme.fg(style.color, style.label);
}

/** Right-aligned PR badge for the Latest Run panel title (empty when no PR). */
function prBadge(theme: ThemeApi, run: LatestRunSummary | null): string {
	if (!run) return "";
	const status = prStatus(run);
	if (status === "none") return "";
	return theme.fg(prStatusStyle(status).color, status === "merged" ? "MERGED" : "PR");
}

function runDetailLines(run: LatestRunSummary): string[] {
	const completed = run.completed.length > 0 ? run.completed.join(", ") : "(none)";
	return [
		`run: ${run.adwId}`,
		`issue: ${run.issueNumber ? `#${run.issueNumber}` : "unknown"}`,
		`runner: ${run.runner ?? "unknown"}`,
		`branch: ${run.branchName ?? "unknown"}`,
		`completed (${run.completed.length}): ${completed}`,
		`pr: ${run.prUrl ? `${run.prUrl} (${prStatus(run)})` : "none"}`,
		`cost: ${run.totalCostUsd === null ? "unknown" : `$${run.totalCostUsd.toFixed(3)}`}`,
		`workspace: agents/${run.adwId}`,
	];
}

type RunDetailAction = "insert" | "back" | "close";

/** Run picker overlay (read-only). Resolves to an adw_id, or null on cancel. */
function pickRun(ctx: ExtensionContext, runs: readonly LatestRunSummary[]): Promise<string | null> {
	return ctx.ui.custom<string | null>((tui, theme, _keybindings, done) => {
		const container = new Container();
		container.addChild(new DynamicBorder((s: string) => theme.fg("accent", s)));
		container.addChild(new Text(theme.fg("accent", theme.bold(`ADW Runs (${runs.length})`)), 1, 0));
		const items: SelectItem[] = runs.map((run) => {
			const last = run.completed.length > 0 ? ` (last: ${run.completed[run.completed.length - 1]})` : "";
			return {
				value: run.adwId,
				label: `${run.adwId}  ${run.issueNumber ? `#${run.issueNumber}` : "#?"}  ${run.runner ?? "?"}`,
				description: `completed ${run.completed.length}${last} · ${prStatus(run) === "none" ? "no PR" : prStatus(run)}`,
			};
		});
		const list = new SelectList(items, Math.min(items.length, 10), getSelectListTheme());
		list.onSelect = (item) => done(item.value);
		list.onCancel = () => done(null);
		container.addChild(list);
		container.addChild(new Text(theme.fg("dim", "↑↓ navigate · enter select · esc cancel"), 1, 0));
		container.addChild(new DynamicBorder((s: string) => theme.fg("accent", s)));
		return {
			render: (w: number) => container.render(w),
			invalidate: () => container.invalidate(),
			handleInput: (d: string) => {
				list.handleInput(d);
				tui.requestRender();
			},
		};
	});
}

/** Run detail overlay (read-only). Resolves to an action, or null on cancel. */
function showRunDetail(ctx: ExtensionContext, run: LatestRunSummary): Promise<RunDetailAction | null> {
	const resumeCmd = resumeCommandFor(run);
	return ctx.ui.custom<RunDetailAction | null>((tui, theme, _keybindings, done) => {
		const container = new Container();
		container.addChild(new DynamicBorder((s: string) => theme.fg("accent", s)));
		container.addChild(new Text(theme.fg("accent", theme.bold(`ADW Run ${run.adwId}`)), 1, 0));
		for (const line of runDetailLines(run)) {
			container.addChild(new Text(theme.fg("muted", line), 1, 0));
		}
		const actions: SelectItem[] = [
			{
				value: "insert",
				label: "Insert resume command into editor",
				description: resumeCmd ?? "unavailable: run has no recorded issue number",
			},
			{ value: "back", label: "Back to run list" },
			{ value: "close", label: "Close" },
		];
		const list = new SelectList(actions, actions.length, getSelectListTheme());
		list.onSelect = (item) => done(item.value as RunDetailAction);
		list.onCancel = () => done(null);
		container.addChild(new Text(theme.fg("dim", "enter select · esc close"), 1, 0));
		container.addChild(list);
		container.addChild(new DynamicBorder((s: string) => theme.fg("accent", s)));
		return {
			render: (w: number) => container.render(w),
			invalidate: () => container.invalidate(),
			handleInput: (d: string) => {
				list.handleInput(d);
				tui.requestRender();
			},
		};
	});
}

// --- /adw-config inspector (Phase 4) ----------------------------------------
//
// A read-only master/detail overlay over .adw/config.json: a SelectList of
// sections whose highlighted entry live-renders its values (via
// SelectList.onSelectionChange) in the mission-control key:value style.

const CONFIG_DOCS = {
	arch: "adw_sdlc/docs/ARCHITECTURE.md",
	universal: "adw_sdlc/docs/UNIVERSAL.md",
} as const;

interface BrowserSection {
	id: string;
	title: string;
	/** Plain-text one-line summary shown in the section list. */
	summary: string;
	/** Marks a section the user should look at (e.g. empty gates). */
	warn: boolean;
	/** Relevant docs surfaced in the detail pane. */
	docs: readonly string[];
	/** Themed detail lines for the right/lower pane. */
	detail: (theme: ThemeApi) => string[];
}

/** Colour a JSON scalar; empty strings read as a warning, not blank. */
function jsonScalar(theme: ThemeApi, v: unknown): string {
	if (v === null || v === undefined) return theme.fg("dim", "null");
	if (typeof v === "boolean") return theme.fg(v ? "success" : "dim", String(v));
	if (typeof v === "number") return theme.fg("text", String(v));
	if (typeof v === "string") return v === "" ? theme.fg("warning", "(empty)") : theme.fg("text", v);
	return theme.fg("text", String(v));
}

/** Pretty-print a JSON subtree into themed key:value lines (objects nest, scalar arrays inline). */
function renderJsonLines(theme: ThemeApi, value: unknown, indent = 0): string[] {
	const pad = "  ".repeat(indent);
	if (value === null || typeof value !== "object") return [`${pad}${jsonScalar(theme, value)}`];
	if (Array.isArray(value)) {
		if (value.length === 0) return [`${pad}${theme.fg("warning", "(empty)")}`];
		if (value.every((v) => v === null || typeof v !== "object")) {
			return [`${pad}${theme.fg("text", value.map((v) => String(v)).join(", "))}`];
		}
		const out: string[] = [];
		value.forEach((v, i) => {
			out.push(`${pad}${theme.fg("dim", `[${i}]`)}`);
			out.push(...renderJsonLines(theme, v, indent + 1));
		});
		return out;
	}
	const entries = Object.entries(value as Record<string, unknown>);
	if (entries.length === 0) return [`${pad}${theme.fg("dim", "(none)")}`];
	const keyW = Math.min(18, Math.max(1, ...entries.map(([k]) => k.length)));
	const out: string[] = [];
	for (const [k, v] of entries) {
		const nested = v !== null && typeof v === "object";
		const scalarArray = Array.isArray(v) && v.every((x) => x === null || typeof x !== "object");
		if (nested && !scalarArray) {
			out.push(`${pad}${theme.fg("accent", k)}`);
			out.push(...renderJsonLines(theme, v, indent + 1));
		} else if (scalarArray) {
			const arr = v as unknown[];
			const val = arr.length ? theme.fg("text", arr.map((x) => String(x)).join(", ")) : theme.fg("warning", "(empty)");
			out.push(`${pad}${theme.fg("dim", k.padEnd(keyW))}${theme.fg("dim", ": ")}${val}`);
		} else {
			out.push(`${pad}${theme.fg("dim", k.padEnd(keyW))}${theme.fg("dim", ": ")}${jsonScalar(theme, v)}`);
		}
	}
	return out;
}

/**
 * Decompose `.adw/config.json` into navigable sections. A null config (missing
 * or malformed) collapses to a single readable "Status" error section so the
 * overlay still renders a clear message rather than failing.
 */
function buildConfigSections(config: Record<string, unknown> | null, cwd: string): BrowserSection[] {
	if (!config) {
		return [
			{
				id: "status",
				title: "Status",
				summary: "config missing or malformed",
				warn: true,
				docs: [CONFIG_DOCS.universal],
				detail: (theme) => [
					theme.fg("warning", "No readable .adw/config.json."),
					theme.fg("muted", "Open this from the repo root; the file may be missing or invalid JSON."),
				],
			},
		];
	}

	const sections: BrowserSection[] = [];
	const project = recordAt(config, "project");
	sections.push({
		id: "project",
		title: "Project",
		summary: `${stringAt(project, "name", "?")} (id ${stringAt(project, "id", "?")})`,
		warn: false,
		docs: [CONFIG_DOCS.universal],
		detail: (theme) => renderJsonLines(theme, { project: config["project"], version: config["version"], progress: config["progress"] }),
	});

	const providers = recordAt(config, "providers");
	sections.push({
		id: "providers",
		title: "Providers",
		summary: `${stringAt(recordAt(providers, "workItems"), "type")}/${stringAt(recordAt(providers, "vcs"), "type")}/${stringAt(recordAt(providers, "changeRequests"), "type")}`,
		warn: false,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) => renderJsonLines(theme, config["providers"]),
	});

	const commands = recordAt(config, "commands");
	const testCmd = typeof commands["defaultTestCommand"] === "string" ? (commands["defaultTestCommand"] as string).trim() : "";
	const finalizeCount = Array.isArray(commands["defaultFinalizeGates"]) ? (commands["defaultFinalizeGates"] as unknown[]).length : 0;
	sections.push({
		id: "commands",
		title: "Commands",
		summary: `test gate ${testCmd || "none"}${testCmd ? "" : " \u26a0"} \u00b7 finalize ${finalizeCount}${finalizeCount ? "" : " \u26a0"}`,
		warn: testCmd === "" || finalizeCount === 0,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) => [
			`${theme.fg("dim", "defaultTestCommand".padEnd(20))}${theme.fg("dim", ": ")}${testCmd ? theme.fg("text", testCmd) : theme.fg("warning", "(none) \u2014 resolve loop has no test gate")}`,
			`${theme.fg("dim", "defaultFinalizeGates".padEnd(20))}${theme.fg("dim", ": ")}${finalizeCount ? theme.fg("text", String(finalizeCount)) : theme.fg("warning", "(empty) \u2014 no finalize gates configured")}`,
		],
	});

	const phasesRaw = config["phases"];
	const configured = Array.isArray(phasesRaw) && phasesRaw.some((p) => typeof p === "string");
	const customPhases = Array.isArray(config["customPhases"])
		? (config["customPhases"] as unknown[]).filter((p): p is string => typeof p === "string")
		: [];
	const chain = configured ? (phasesRaw as unknown[]).map((p) => String(p)) : [...DEFAULT_AGENT_PHASES];
	sections.push({
		id: "phases",
		title: "Phases",
		summary: configured ? `${chain.length} configured` : `${chain.length} (default catalog)`,
		warn: false,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) => [
			theme.fg("dim", configured ? "chain (config.phases):" : "chain (default catalog \u2014 no config.phases):"),
			`  ${theme.fg("text", chain.join(" \u2192 "))}`,
			`${theme.fg("dim", "custom phases".padEnd(14))}${theme.fg("dim", ": ")}${customPhases.length ? theme.fg("text", customPhases.join(", ")) : theme.fg("dim", "(none)")}`,
			theme.fg("muted", "runs as setup \u2192 <chain> \u2192 merge; finalize/ci-fix/report are kernel wrappers."),
		],
	});

	const models = recordAt(config, "models");
	sections.push({
		id: "models",
		title: "Models",
		summary: `classify ${stringAt(models, "classifyModel", "?")} \u00b7 tier ${stringAt(models, "defaultTier", "?")}`,
		warn: false,
		docs: [CONFIG_DOCS.universal],
		detail: (theme) => renderJsonLines(theme, config["models"]),
	});

	const gates = recordAt(config, "gates");
	sections.push({
		id: "gates",
		title: "Gates",
		summary: Object.keys(gates).join(", ") || "(none)",
		warn: false,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) => renderJsonLines(theme, config["gates"]),
	});

	sections.push({
		id: "branching",
		title: "Branching",
		summary: `prefix ${stringAt(recordAt(config, "branching"), "defaultPrefix", "?")}`,
		warn: false,
		docs: [CONFIG_DOCS.universal],
		detail: (theme) => renderJsonLines(theme, config["branching"]),
	});

	const prompts = recordAt(config, "prompts");
	sections.push({
		id: "prompts",
		title: "Prompts",
		summary: stringAt(prompts, "defaultRoot", "?"),
		warn: false,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) => renderJsonLines(theme, config["prompts"]),
	});

	let schemaFiles: string[] = [];
	try {
		const dir = join(cwd, ".adw", "schemas");
		schemaFiles = existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".json")) : [];
	} catch {
		schemaFiles = [];
	}
	sections.push({
		id: "schemas",
		title: "Schemas",
		summary: schemaFiles.length ? `${schemaFiles.length} override(s)` : "built-in only",
		warn: false,
		docs: [CONFIG_DOCS.arch],
		detail: (theme) =>
			schemaFiles.length
				? [theme.fg("dim", ".adw/schemas/ overrides:"), ...schemaFiles.map((f) => `  ${theme.fg("text", f)}`)]
				: [theme.fg("dim", "No .adw/schemas/ overrides; phases use the built-in result schemas.")],
	});

	return sections;
}

/**
 * Live master/detail browser overlay (read-only) shared by /adw-config and
 * /adw-mvp: a section list whose highlighted entry live-renders its themed
 * detail lines. Resolves to null when closed.
 */
function showSectionBrowser(
	ctx: ExtensionContext,
	heading: string,
	subtitle: string,
	sections: readonly BrowserSection[],
): Promise<null> {
	const byId = new Map(sections.map((s) => [s.id, s]));
	const MAX_DETAIL = 14;
	return ctx.ui.custom<null>((tui, theme, _keybindings, done) => {
		const items: SelectItem[] = sections.map((s, i) => ({
			value: s.id,
			label: `${i + 1}. ${s.title}${s.warn ? "  \u26a0" : ""}`,
			description: s.summary,
		}));
		const list = new SelectList(items, Math.min(items.length, 9), getSelectListTheme());
		list.onCancel = () => done(null);
		list.onSelectionChange = () => tui.requestRender();
		const rule = (w: number) => theme.fg("border", BOX.h.repeat(Math.max(1, w)));
		return {
			invalidate: () => list.invalidate(),
			handleInput: (d: string) => {
				list.handleInput(d);
				tui.requestRender();
			},
			render: (width: number): string[] => {
				const w = Math.max(20, width);
				const out: string[] = [rule(w)];
				out.push(`${theme.fg("accent", theme.bold(heading))} ${theme.fg("dim", subtitle)}`);
				out.push(...list.render(w));
				out.push(rule(w));
				const sel = list.getSelectedItem();
				const section = sel ? byId.get(sel.value) : undefined;
				if (section) {
					out.push(`${theme.fg("accent", theme.bold(section.title.toUpperCase()))}`);
					const detail = section.detail(theme);
					for (const line of detail.slice(0, MAX_DETAIL)) out.push(`  ${line}`);
					if (detail.length > MAX_DETAIL) {
						out.push(`  ${theme.fg("dim", `\u2026 ${detail.length - MAX_DETAIL} more`)}`);
					}
					if (section.docs.length > 0) {
						out.push(`  ${theme.fg("dim", "docs: ")}${theme.fg("muted", section.docs.join("  "))}`);
					}
				}
				out.push(rule(w));
				out.push(theme.fg("dim", "\u2191\u2193 navigate \u00b7 esc close"));
				return out;
			},
		};
	});
}

function tail(text: string, limit = COMMAND_OUTPUT_LIMIT): string {
	return text.length <= limit ? text : text.slice(text.length - limit);
}

function displayCommand(command: string, args: readonly string[]): string {
	return [command, ...args].join(" ");
}

function formatCommandSummary(summary: CommandSummary | null): string | null {
	if (!summary) return null;
	const icon = summary.code === 0 && !summary.killed ? "✓" : "✗";
	const seconds = (summary.durationMs / 1000).toFixed(1);
	const when = new Date(summary.timestamp).toLocaleTimeString();
	// The widget prefixes a bold `LAST` label, so this returns just the payload.
	return `${icon} ${summary.label} · code ${summary.code}${summary.killed ? " · killed" : ""} · ${seconds}s · ${when}`;
}

function formatCommandDetails(summary: CommandSummary): string {
	const parts = [
		`$ ${summary.command}`,
		`exit: ${summary.code}${summary.killed ? " (killed)" : ""}`,
		`duration: ${(summary.durationMs / 1000).toFixed(1)}s`,
	];
	const stdout = tail(summary.stdout.trim());
	const stderr = tail(summary.stderr.trim());
	if (stdout) parts.push(`\nstdout:\n${stdout}`);
	if (stderr) parts.push(`\nstderr:\n${stderr}`);
	return parts.join("\n");
}

type PhaseStatus = "done" | "current" | "pending";
interface PhaseCell {
	name: string;
	status: PhaseStatus;
}

/**
 * Canonical ordered phase chain for display: setup → agent chain → merge.
 * Any completed phase the chain does not already list (defensive: a custom or
 * reordered run) is appended so real progress is never hidden.
 */
function canonicalPhaseChain(agentPhases: readonly string[], completed: readonly string[]): string[] {
	const chain = ["setup", ...agentPhases, "merge"];
	for (const phase of completed) {
		if (!chain.includes(phase)) chain.push(phase);
	}
	return chain;
}

/**
 * Resolve each phase to done/current/pending. The first not-yet-completed
 * phase is the "current" one (when markCurrent is set); a fully-completed run
 * has no current phase. Conditional phases (e2e/document) are recorded as
 * completed even when their gate skips them, so a skipped gate reads as done —
 * exactly what the orchestrator persists.
 */
function phaseCells(chain: readonly string[], completed: readonly string[], markCurrent: boolean): PhaseCell[] {
	const done = new Set(completed);
	let currentTaken = false;
	return chain.map((name) => {
		if (done.has(name)) return { name, status: "done" };
		if (markCurrent && !currentTaken) {
			currentTaken = true;
			return { name, status: "current" };
		}
		return { name, status: "pending" };
	});
}

// --- mission-control dashboard rendering ------------------------------------
//
// Aesthetic (see .impeccable.md): a dark mission-control panel — numbered,
// titled, box-drawn sections; dim key columns against status-coloured values;
// status dots and green completion meters. The widget returns a custom
// component so it can draw real boxes sized to the editor width.

const BOX = { tl: "┌", tr: "┐", bl: "└", br: "┘", h: "─", v: "│" } as const;
const SEP = "  ·  ";

/** Pad/truncate a pre-styled (ANSI-aware) string to exactly `w` columns. */
function fit(s: string, w: number): string {
	return truncateToWidth(s, Math.max(0, w), "…", true);
}

/** A status dot + label, coloured by phase status (done/current/pending). */
function phaseDot(theme: ThemeApi, cell: PhaseCell): string {
	if (cell.status === "done") return `${theme.fg("success", "●")} ${theme.fg("text", cell.name)}`;
	if (cell.status === "current") return `${theme.fg("warning", "◉")} ${theme.bold(theme.fg("accent", cell.name))}`;
	return `${theme.fg("dim", "○")} ${theme.fg("dim", cell.name)}`;
}

/** Green completion meter: ▕████░░░░▏. */
function bar(theme: ThemeApi, frac: number, width: number): string {
	const filled = Math.max(0, Math.min(width, Math.round(frac * width)));
	return (
		`${theme.fg("dim", "▕")}${theme.fg("success", "█".repeat(filled))}` +
		`${theme.fg("dim", "░".repeat(width - filled))}${theme.fg("dim", "▏")}`
	);
}

/** `key : value` row — dim padded key, pre-styled value. */
function kv(theme: ThemeApi, key: string, value: string, keyW: number): string {
	return `${theme.fg("dim", key.padEnd(keyW))}${theme.fg("dim", ": ")}${value}`;
}

/**
 * Render a numbered, titled, box-drawn panel to exactly `width` columns. `rows`
 * are pre-styled content strings (fit to the inner width); `rightTag` is
 * right-aligned in the title row (e.g. a count or badge).
 */
function panel(
	theme: ThemeApi,
	width: number,
	num: number,
	title: string,
	rightTag: string,
	rows: readonly string[],
): string[] {
	const b = (s: string) => theme.fg("border", s);
	const inner = Math.max(1, width - 4);
	const frameW = Math.max(0, width - 2);
	const out: string[] = [b(BOX.tl + BOX.h.repeat(frameW) + BOX.tr)];
	const titlePlain = `${num}. ${title.toUpperCase()}`;
	const gap = Math.max(1, inner - visibleWidth(titlePlain) - visibleWidth(rightTag));
	const titleContent = `${theme.fg("accent", theme.bold(titlePlain))}${" ".repeat(gap)}${rightTag}`;
	out.push(`${b(BOX.v)} ${fit(titleContent, inner)} ${b(BOX.v)}`);
	for (const row of rows) {
		out.push(`${b(BOX.v)} ${fit(row, inner)} ${b(BOX.v)}`);
	}
	out.push(b(BOX.bl + BOX.h.repeat(frameW) + BOX.br));
	return out;
}

/** Place two equal-width panels side by side with a one-column gap. */
function sideBySide(a: readonly string[], aW: number, b: readonly string[], bW: number): string[] {
	const n = Math.max(a.length, b.length);
	const out: string[] = [];
	for (let i = 0; i < n; i++) {
		out.push(`${a[i] ?? " ".repeat(aW)} ${b[i] ?? " ".repeat(bW)}`);
	}
	return out;
}

/** Arrange phase cells into a status-dot grid of `cols` columns. */
function phaseGrid(theme: ThemeApi, cells: readonly PhaseCell[], inner: number, cols: number): string[] {
	const colW = Math.max(10, Math.floor(inner / cols));
	const rows: string[] = [];
	for (let i = 0; i < cells.length; i += cols) {
		const slice = cells.slice(i, i + cols);
		rows.push(slice.map((c, j) => (j < slice.length - 1 ? fit(phaseDot(theme, c), colW) : phaseDot(theme, c))).join(""));
	}
	return rows;
}

/** The whole cockpit: heading + numbered panels, drawn to the editor width. */
function renderDashboard(theme: ThemeApi, model: DashboardModel, rawWidth: number): string[] {
	const width = Math.max(24, rawWidth);
	const lines: string[] = [];
	const time = new Date(model.updatedAt).toLocaleTimeString();

	// Heading: ADW COCKPIT ............ updated HH:MM:SS  ● live
	const heading = theme.fg("accent", theme.bold("ADW COCKPIT"));
	const statusRight = `${theme.fg("dim", "updated ")}${theme.fg("muted", time)}  ${theme.fg("success", "●")} ${theme.fg("dim", "live")}`;
	const hGap = Math.max(1, width - visibleWidth("ADW COCKPIT") - visibleWidth(`updated ${time}  ● live`));
	lines.push(fit(`${heading}${" ".repeat(hGap)}${statusRight}`, width));

	if (!model.config) {
		lines.push(
			...panel(theme, width, 1, "Status", "", [
				kv(theme, "status", theme.fg("warning", "no project pack"), 10),
				theme.fg("muted", model.noConfigHint ?? ""),
			]),
		);
		return lines;
	}

	const dirtyColor = model.dirtyCount === 0 ? "success" : "warning";
	const gateMissing = model.config.testGate === "none";
	const overviewRows = [
		kv(theme, "runtime", theme.fg("text", "Pi"), 10),
		kv(theme, "project", theme.fg("accent", model.config.projectName), 10),
		kv(theme, "providers", theme.fg("text", model.config.providerSummary), 10),
		kv(theme, "prompts", theme.fg("text", model.config.promptsRoot), 10),
		kv(theme, "branch", theme.fg("text", model.branch), 10),
		kv(theme, "worktree", theme.fg(dirtyColor, model.dirty), 10),
		kv(theme, "test gate", theme.fg(gateMissing ? "warning" : "success", model.config.testGate), 10),
		kv(theme, "finalize", theme.fg("text", `${model.config.finalizeGateCount} gates`), 10),
	];
	// Right-aligned OVERVIEW tag mirrors the evolved prompt-pack: ✓ when a
	// generated pack profile (.adw/pack.profile.json) is present, · otherwise.
	const overviewTag = model.config.hasPack ? theme.fg("success", "pack ✓") : theme.fg("dim", "pack ·");

	const run = model.run;
	const runRows = run
		? [
				kv(theme, "run", theme.fg("accent", run.adwId), 10),
				kv(theme, "issue", theme.fg("text", run.issueNumber ? `#${run.issueNumber}` : "unknown"), 10),
				kv(theme, "runner", theme.fg("text", run.runner ?? "unknown"), 10),
				kv(theme, "pr", prCell(theme, run), 10),
				kv(theme, "cost", run.totalCostUsd === null ? theme.fg("dim", "unknown") : theme.fg("text", `$${run.totalCostUsd.toFixed(3)}`), 10),
			]
		: [
				kv(theme, "run", theme.fg("dim", "no runs yet"), 10),
				kv(theme, "issue", theme.fg("dim", "—"), 10),
				kv(theme, "runner", theme.fg("dim", "—"), 10),
				kv(theme, "pr", theme.fg("dim", "—"), 10),
				kv(theme, "cost", theme.fg("dim", "—"), 10),
			];
	const runTag = prBadge(theme, run);

	// Top band: two panels side by side when wide enough, else stacked.
	if (width >= 84) {
		const aW = Math.floor((width - 1) / 2);
		const bW = width - 1 - aW;
		// Pad the shorter panel so both boxes are the same height and align.
		const rowN = Math.max(overviewRows.length, runRows.length);
		const padRows = (rows: string[]) => [...rows, ...Array(rowN - rows.length).fill("")];
		lines.push(
			...sideBySide(
				panel(theme, aW, 1, "Overview", overviewTag, padRows(overviewRows)),
				aW,
				panel(theme, bW, 2, "Latest Run", runTag, padRows(runRows)),
				bW,
			),
		);
	} else {
		lines.push(...panel(theme, width, 1, "Overview", overviewTag, overviewRows));
		lines.push(...panel(theme, width, 2, "Latest Run", runTag, runRows));
	}

	// Pipeline panel: completion meter + a status-dot grid of every phase.
	const cells = phaseCells(model.phaseChain, run ? run.completed : [], !!run);
	const done = cells.filter((c) => c.status === "done").length;
	const total = cells.length;
	const current = cells.find((c) => c.status === "current");
	const inner = Math.max(1, width - 4);
	const meterTail = current
		? `${theme.fg("dim", SEP)}${theme.fg("warning", "◉")} ${theme.bold(theme.fg("accent", current.name.toUpperCase()))}`
		: run
			? `${theme.fg("dim", SEP)}${theme.bold(theme.fg("success", "✦ ALL CLEAR"))}`
			: `${theme.fg("dim", SEP)}${theme.fg("dim", "pipeline preview")}`;
	const meterRow = `${bar(theme, total ? done / total : 0, 18)} ${theme.bold(theme.fg("text", String(done)))}${theme.fg("dim", `/${total}`)}${meterTail}`;
	const cols = width >= 104 ? 4 : width >= 64 ? 3 : 2;
	lines.push(...panel(theme, width, 3, "Pipeline", theme.fg("dim", `${done}/${total}`), [meterRow, ...phaseGrid(theme, cells, inner, cols)]));

	// Last command (slim status line under the panels).
	if (model.commandLine) {
		const ok = !model.commandLine.includes("✗");
		lines.push(fit(`${theme.fg("dim", "last  ")}${theme.fg(ok ? "success" : "warning", model.commandLine)}`, width));
	}
	return lines;
}

interface DashboardModel {
	config: AdwConfigSummary | null;
	branch: string;
	dirty: string;
	dirtyCount: number;
	/** The latest run (newest state.json by mtime), or null when none exist. */
	run: LatestRunSummary | null;
	/** Full canonical phase chain (setup → agent chain → merge) for rendering. */
	phaseChain: string[];
	/** Guidance shown only when there is no usable `.adw/config.json`. */
	noConfigHint: string | null;
	commandLine: string | null;
	/** When the model was built (drives the heading's `updated HH:MM:SS`). */
	updatedAt: number;
}

function buildDashboardModel(ctx: ExtensionContext): { model: DashboardModel; status: string } {
	const config = readConfigSummary(ctx.cwd);
	if (!config) {
		return {
			model: {
				config: null,
				branch: "unknown",
				dirty: "unknown",
				dirtyCount: 0,
				run: null,
				phaseChain: [],
				noConfigHint: "Open this from the repository root to inspect ADW state.",
				commandLine: null,
				updatedAt: Date.now(),
			},
			status: ctx.ui.theme.fg("warning", "ADW no-config"),
		};
	}

	const git = readGitSummary(ctx.cwd);
	const dirty = git.dirtyCount === 0 ? "clean" : `${git.dirtyCount} changed`;
	const statusColor = git.dirtyCount === 0 ? "success" : "warning";
	const run = latestRun(ctx.cwd);
	return {
		model: {
			config,
			branch: git.branch,
			dirty,
			dirtyCount: git.dirtyCount,
			run,
			phaseChain: canonicalPhaseChain(config.agentPhases, run?.completed ?? []),
			noConfigHint: null,
			commandLine: formatCommandSummary(lastCommandSummary),
			updatedAt: Date.now(),
		},
		status: `${ctx.ui.theme.fg("accent", "ADW")} ${config.projectName} · ${ctx.ui.theme.fg(statusColor, dirty)}`,
	};
}

function dashboardWidget(model: DashboardModel) {
	// A hand-rolled component so the cockpit can draw box panels sized to the
	// live editor width (string[] widgets cannot; Container rows cannot box).
	return (_tui: unknown, theme: ExtensionContext["ui"]["theme"]) => ({
		invalidate() {},
		render: (width: number): string[] => renderDashboard(theme, model, width),
	});
}

function commandHintWidget() {
	return (_tui: unknown, theme: ExtensionContext["ui"]["theme"]) => {
		const cmd = (name: string, desc: string) =>
			`${theme.fg("dim", "▸ ")}${theme.bold(theme.fg("accent", name))} ${theme.fg("dim", desc)}`;
		const hint = [
			cmd("/adw-menu", "palette \u00b7 ctrl+shift+a"),
			cmd("/adw-runs", "inspect"),
			cmd("/adw-config", "config"),
			cmd("/adw-mvp", "readiness"),
			cmd("/adw-run <id>", "guarded"),
		].join(theme.fg("dim", "   "));
		return new Text(hint, 1, 0);
	};
}

/**
 * A point-in-time footer snapshot. Built once per install/event (session_start,
 * agent_end, command completion, /adw-refresh, /adw-footer) so the per-frame
 * footer render stays pure string-formatting — no git subprocess or disk read
 * on every paint.
 */
interface FooterSnapshot {
	project: string | null;
	branch: string;
	dirty: string;
	dirtyCount: number;
	phaseDone: number;
	phaseTotal: number;
	currentPhase: string | null;
	hasRun: boolean;
	run: LatestRunSummary | null;
}

function buildFooterSnapshot(ctx: ExtensionContext): FooterSnapshot {
	const config = readConfigSummary(ctx.cwd);
	const git = readGitSummary(ctx.cwd);
	const run = latestRun(ctx.cwd);
	const chain = config ? canonicalPhaseChain(config.agentPhases, run?.completed ?? []) : [];
	const cells = phaseCells(chain, run ? run.completed : [], !!run);
	return {
		project: config?.projectName ?? null,
		branch: git.branch,
		dirty: git.dirtyCount === 0 ? "clean" : `${git.dirtyCount} changed`,
		dirtyCount: git.dirtyCount,
		phaseDone: cells.filter((c) => c.status === "done").length,
		phaseTotal: cells.length,
		currentPhase: cells.find((c) => c.status === "current")?.name ?? null,
		hasRun: !!run,
		run,
	};
}

/**
 * The mission-control status bar: dim UPPERCASE labels against status-coloured
 * values, separated by faint dividers, with a health dot, a pipeline mini-bar,
 * and a right-justified run/cost cluster. Mirrors the reference footer.
 */
function renderFooterBar(theme: ThemeApi, snap: FooterSnapshot, branch: string, width: number): string {
	const divider = theme.fg("border", " │ ");
	const seg = (lbl: string, value: string) => `${theme.fg("dim", lbl)} ${value}`;
	const healthColor = snap.project === null || snap.dirtyCount > 0 ? "warning" : "success";

	const left: string[] = [`${theme.fg(healthColor, "●")} ${theme.bold(theme.fg("accent", "ADW"))}`];
	left.push(seg("PROJECT", theme.fg("text", snap.project ?? "no-config")));
	left.push(seg("BRANCH", theme.fg("text", branch)));
	left.push(seg("TREE", theme.fg(snap.dirtyCount === 0 ? "success" : "warning", snap.dirty)));
	if (snap.phaseTotal > 0) {
		const frac = snap.phaseDone / snap.phaseTotal;
		const tail = snap.currentPhase
			? ` ${theme.fg("warning", snap.currentPhase)}`
			: snap.hasRun
				? ` ${theme.fg("success", "clear")}`
				: "";
		left.push(
			`${theme.fg("dim", "PIPELINE")} ${bar(theme, frac, 8)} ${theme.fg("text", `${snap.phaseDone}/${snap.phaseTotal}`)}${tail}`,
		);
	}
	if (lastCommandSummary) {
		const ok = lastCommandSummary.code === 0 && !lastCommandSummary.killed;
		left.push(seg("LAST", theme.fg(ok ? "success" : "warning", `${ok ? "✓" : "✗"} ${lastCommandSummary.label}`)));
	}

	let right = "";
	if (snap.run) {
		const cost =
			snap.run.totalCostUsd === null ? "" : `  ${theme.fg("muted", `$${snap.run.totalCostUsd.toFixed(3)}`)}`;
		right = `${theme.fg("dim", "RUN")} ${theme.fg("accent", snap.run.adwId)}${cost}`;
	}

	const leftStr = left.join(divider);
	const gap = " ".repeat(Math.max(1, width - visibleWidth(leftStr) - visibleWidth(right)));
	return truncateToWidth(leftStr + gap + right, width, "");
}

function applyFooter(ctx: ExtensionContext, enabled: boolean): void {
	if (!ctx.hasUI) return;
	if (!enabled) {
		ctx.ui.setFooter(undefined);
		return;
	}
	// Snapshot once per install (this runs on each refresh-triggering event), so
	// the per-frame render below never touches git or disk.
	const snapshot = buildFooterSnapshot(ctx);
	ctx.ui.setFooter((_tui, theme, footerData) => ({
		invalidate() {},
		render: (width: number): string[] => [
			renderFooterBar(theme, snapshot, footerData.getGitBranch() ?? snapshot.branch, width),
		],
	}));
}

function refresh(ctx: ExtensionContext): void {
	if (!ctx.hasUI) return;
	const { model, status } = buildDashboardModel(ctx);
	ctx.ui.setWidget(WIDGET_ID, dashboardWidget(model));
	ctx.ui.setWidget(HELP_WIDGET_ID, commandHintWidget(), { placement: "belowEditor" });
	ctx.ui.setStatus(STATUS_ID, status);
}

function clear(ctx: ExtensionContext): void {
	if (!ctx.hasUI) return;
	ctx.ui.setWidget(WIDGET_ID, undefined);
	ctx.ui.setWidget(HELP_WIDGET_ID, undefined);
	ctx.ui.setStatus(STATUS_ID, undefined);
}

function adwPackageDir(ctx: ExtensionContext): string {
	return join(ctx.cwd, "adw_sdlc");
}

function syntheticExecResult(code: number, stderr: string, killed = false): ExecResult {
	return { stdout: "", stderr, code, killed };
}

async function runWithOptionalLoader(
	pi: ExtensionAPI,
	ctx: ExtensionContext,
	label: string,
	command: string,
	args: string[],
	options: { cwd: string; timeout: number },
): Promise<ExecResult> {
	if (ctx.mode !== "tui") {
		try {
			return await pi.exec(command, args, { cwd: options.cwd, timeout: options.timeout });
		} catch (err) {
			return syntheticExecResult(1, err instanceof Error ? err.message : String(err));
		}
	}

	const result = await ctx.ui.custom<ExecResult | null>((tui, theme, _keybindings, done) => {
		let finished = false;
		const finish = (value: ExecResult | null) => {
			if (!finished) {
				finished = true;
				done(value);
			}
		};
		const loader = new BorderedLoader(tui, theme, `Running ${label}...`, { cancellable: true });
		loader.onAbort = () => finish(null);
		pi.exec(command, args, { cwd: options.cwd, timeout: options.timeout, signal: loader.signal })
			.then((value) => finish(value))
			.catch((err) => finish(syntheticExecResult(1, err instanceof Error ? err.message : String(err))));
		return loader;
	});

	return result ?? syntheticExecResult(130, "Command cancelled by user", true);
}

async function runAndRecord(
	pi: ExtensionAPI,
	ctx: ExtensionContext,
	label: string,
	command: string,
	args: string[],
	options: { cwd: string; timeout: number },
): Promise<CommandSummary> {
	const start = Date.now();
	const result = await runWithOptionalLoader(pi, ctx, label, command, args, options);
	const summary: CommandSummary = {
		label,
		command: displayCommand(command, args),
		code: result.code,
		killed: result.killed,
		durationMs: Date.now() - start,
		timestamp: Date.now(),
		stdout: result.stdout,
		stderr: result.stderr,
	};
	lastCommandSummary = summary;
	return summary;
}

function notifyCommandResult(ctx: ExtensionContext, summary: CommandSummary): void {
	const ok = summary.code === 0 && !summary.killed;
	ctx.ui.notify(`${summary.label} ${ok ? "passed" : "failed"} (code ${summary.code})`, ok ? "info" : "warning");
	ctx.ui.notify(formatCommandDetails(summary), ok ? "info" : "warning");
}

type CheckName = "typecheck" | "lint-env" | "pack-check" | "test" | "build";

const CHECKS: Record<CheckName, { label: string; command: string; args: string[]; timeout: number }> = {
	typecheck: { label: "typecheck", command: "npm", args: ["run", "typecheck"], timeout: 120_000 },
	"lint-env": { label: "lint:env", command: "npm", args: ["run", "lint:env"], timeout: 60_000 },
	// Verifies the generated prompt-pack matches templates+profile (read-only:
	// `pack-generate --check` writes nothing). Added when the repo moved to
	// template-generated project prompt packs.
	"pack-check": { label: "pack:check", command: "npm", args: ["run", "pack:check"], timeout: 60_000 },
	test: { label: "test", command: "npm", args: ["test"], timeout: 180_000 },
	build: { label: "build", command: "npm", args: ["run", "build"], timeout: 120_000 },
};

function parseWorkItemId(args: string): string | null {
	const id = args.trim().split(/\s+/)[0] ?? "";
	return /^\d+$/.test(id) ? id : null;
}

/** Tab-completion for a command's fixed option list, filtered by the typed prefix. */
function argCompletions(options: readonly string[], prefix: string): AutocompleteItem[] {
	const p = prefix.trim().toLowerCase();
	return options.filter((o) => o.startsWith(p)).map((o) => ({ value: o, label: o }));
}

// --- Phase 5: workflow assistant --------------------------------------------

/** MVP-readiness docs summarised by the /adw-mvp panel. */
const MVP_DOCS = ["PARITY.md", "MVP-READINESS.md", "HANDOVER.md"] as const;

/** Read the three MVP docs from adw_sdlc/ into browser sections (status tallies + headings). */
function buildMvpSections(cwd: string): BrowserSection[] {
	return MVP_DOCS.map((file) => {
		const id = file.replace(/\.md$/, "");
		const docPath = join(cwd, "adw_sdlc", file);
		let text: string | null = null;
		try {
			text = existsSync(docPath) ? readFileSync(docPath, "utf8") : null;
		} catch {
			text = null;
		}
		if (text === null) {
			return {
				id,
				title: id,
				summary: "missing",
				warn: true,
				docs: [`adw_sdlc/${file}`],
				detail: (theme) => [theme.fg("warning", `Not found: adw_sdlc/${file}`)],
			};
		}
		const body = text;
		const lines = body.split("\n");
		const title = (lines.find((l) => l.startsWith("# ")) ?? file).replace(/^#\s*/, "");
		const tally = (re: RegExp) => (body.match(re) ?? []).length;
		const done = tally(/\u2705/g);
		const owed = tally(/\u23f3/g);
		const notStarted = tally(/\u274c/g);
		const auto = tally(/\ud83d\udd27/g);
		const headings = lines.filter((l) => /^##\s/.test(l)).map((l) => l.replace(/^##\s*/, "")).slice(0, 8);
		return {
			id,
			title: id,
			summary: `\u2705${done} \u23f3${owed} \u274c${notStarted}${auto ? ` \ud83d\udd27${auto}` : ""} \u00b7 ${lines.length} ln`,
			warn: owed > 0 || notStarted > 0,
			docs: [`adw_sdlc/${file}`],
			detail: (theme) => [
				`${theme.fg("dim", "title".padEnd(8))}${theme.fg("dim", ": ")}${theme.fg("text", title)}`,
				`${theme.fg("dim", "status".padEnd(8))}${theme.fg("dim", ": ")}${theme.fg("success", `\u2705 ${done} done`)}  ${theme.fg("warning", `\u23f3 ${owed} owed`)}  ${theme.fg("error", `\u274c ${notStarted}`)}  ${theme.fg("accent", `\ud83d\udd27 ${auto}`)}`,
				`${theme.fg("dim", "lines".padEnd(8))}${theme.fg("dim", ": ")}${theme.fg("text", String(lines.length))}`,
				theme.fg("dim", "sections:"),
				...headings.map((h) => `  ${theme.fg("text", h)}`),
			],
		};
	});
}

// --- shared flows (callable from both a command and the /adw-menu palette) ---

/** Human-readable message for a caught unknown error. */
function errText(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

/**
 * Run an interactive flow, surfacing any thrown error as a notification rather
 * than letting it crash the extension (an overlay/IO failure must not take the
 * cockpit down).
 */
async function safely(ctx: ExtensionContext, label: string, fn: () => Promise<void>): Promise<void> {
	try {
		await fn();
	} catch (err) {
		ctx.ui.notify(`${label} failed: ${errText(err)}`, "error");
	}
}

async function configFlow(ctx: ExtensionContext): Promise<void> {
	await safely(ctx, "Inspect config", async () => {
		// parseJsonObject returns null for a missing OR malformed file; the builder
		// turns that into a single readable error section.
		const config = parseJsonObject(join(ctx.cwd, ".adw", "config.json"));
		const sections = buildConfigSections(config, ctx.cwd);
		if (ctx.mode !== "tui") {
			if (!config) {
				ctx.ui.notify("No readable .adw/config.json (missing or malformed).", "warning");
				return;
			}
			const warnings = sections.filter((s) => s.warn).map((s) => s.title);
			ctx.ui.notify(
				`ADW config: ${sections.length} sections (${sections.map((s) => s.title).join(", ")}).` +
					`${warnings.length ? ` Warnings: ${warnings.join(", ")}.` : ""} Open in TUI to browse.`,
				warnings.length ? "warning" : "info",
			);
			return;
		}
		await showSectionBrowser(ctx, "ADW CONFIG", "\u00b7 .adw/config.json", sections);
	});
}

async function mvpFlow(ctx: ExtensionContext): Promise<void> {
	await safely(ctx, "MVP readiness", async () => {
		const sections = buildMvpSections(ctx.cwd);
		if (ctx.mode !== "tui") {
			const summary = sections.map((s) => `${s.title} ${s.summary}`).join(" \u00b7 ");
			ctx.ui.notify(`ADW MVP readiness \u2014 ${summary}. Open in TUI to browse.`, sections.some((s) => s.warn) ? "warning" : "info");
			return;
		}
		await showSectionBrowser(ctx, "ADW MVP READINESS", "\u00b7 adw_sdlc/", sections);
	});
}

async function runsFlow(ctx: ExtensionContext): Promise<void> {
	await safely(ctx, "Browse runs", async () => {
		// Non-TUI UI modes (rpc/json/print) cannot host a custom overlay; fall back
		// to a one-line summary notification instead of failing.
		if (ctx.mode !== "tui") {
			const runs = listRuns(ctx.cwd);
			if (runs.length === 0) {
				ctx.ui.notify("No ADW runs found under agents/", "info");
				return;
			}
			const latest = runs[0]!;
			ctx.ui.notify(
				`ADW runs: ${runs.length}; latest ${latest.adwId}${latest.issueNumber ? ` (#${latest.issueNumber})` : ""}. Open in TUI to browse.`,
				"info",
			);
			return;
		}
		// TUI flow: list -> detail -> (optional) insert resume command. Re-reads runs
		// each pass so "Back to run list" reflects the latest state.
		for (;;) {
			const runs = listRuns(ctx.cwd);
			if (runs.length === 0) {
				ctx.ui.notify("No ADW runs found under agents/", "info");
				return;
			}
			const runId = await pickRun(ctx, runs);
			if (!runId) return;
			const run = runs.find((r) => r.adwId === runId);
			if (!run) return;
			const action = await showRunDetail(ctx, run);
			if (action === "back") continue;
			if (action === "insert") {
				const cmd = resumeCommandFor(run);
				if (!cmd) {
					ctx.ui.notify(`Run ${run.adwId} has no recorded issue number; cannot build a resume command`, "warning");
					return;
				}
				ctx.ui.setEditorText(cmd);
				ctx.ui.notify("Resume command inserted into the editor (not executed)", "info");
				return;
			}
			return;
		}
	});
}

// --- command cores (flag-free; callers handle widget/footer refresh) --------

async function execDryRun(pi: ExtensionAPI, ctx: ExtensionContext, workItemId: string): Promise<void> {
	const summary = await runAndRecord(pi, ctx, `dry-run #${workItemId}`, "npx", ["tsx", "src/cli.ts", workItemId, "--dry-run"], {
		cwd: adwPackageDir(ctx),
		timeout: 60_000,
	});
	notifyCommandResult(ctx, summary);
}

async function execChecks(pi: ExtensionAPI, ctx: ExtensionContext, requested: string): Promise<void> {
	// Accept the npm-script spellings (`lint:env`, `pack:check`) as aliases.
	const normalized = requested === "lint:env" ? "lint-env" : requested === "pack:check" ? "pack-check" : requested;
	const names: CheckName[] = normalized === "all" ? ["typecheck", "lint-env", "pack-check", "test", "build"] : [normalized as CheckName];
	if (names.some((name) => CHECKS[name] === undefined)) {
		ctx.ui.notify("Usage: /adw-check typecheck|lint-env|pack-check|test|build|all", "warning");
		return;
	}
	const summaries: CommandSummary[] = [];
	for (const name of names) {
		const check = CHECKS[name];
		const summary = await runAndRecord(pi, ctx, check.label, check.command, check.args, {
			cwd: adwPackageDir(ctx),
			timeout: check.timeout,
		});
		summaries.push(summary);
		if (summary.code !== 0 || summary.killed) break;
	}
	const failed = summaries.find((summary) => summary.code !== 0 || summary.killed);
	if (normalized === "all") {
		const label = failed ? `all stopped at ${failed.label}` : "all checks";
		lastCommandSummary = {
			label,
			command: summaries.map((summary) => summary.command).join(" && "),
			code: failed?.code ?? 0,
			killed: failed?.killed ?? false,
			durationMs: summaries.reduce((total, summary) => total + summary.durationMs, 0),
			timestamp: Date.now(),
			stdout: summaries.map((summary) => `# ${summary.label}\n${summary.stdout}`).join("\n"),
			stderr: summaries.map((summary) => `# ${summary.label}\n${summary.stderr}`).join("\n"),
		};
		notifyCommandResult(ctx, lastCommandSummary);
	} else if (summaries[0]) {
		notifyCommandResult(ctx, summaries[0]);
	}
}

/**
 * The one mutating affordance: start a REAL ADW run. Requires an explicit
 * confirmation that spells out the git/gh/network/PR side effects, and only
 * ever executes from an explicit command/menu action (never an ambient refresh).
 * Non-TUI modes (which cannot host the confirm dialog safely) refuse to run and
 * stage the command instead.
 */
async function execGuardedRun(pi: ExtensionAPI, ctx: ExtensionContext, workItemId: string): Promise<void> {
	const shell = `cd adw_sdlc && npm run issue -- ${workItemId}`;
	if (ctx.mode !== "tui") {
		ctx.ui.notify(`A real ADW run is not started from non-TUI mode. Run it yourself: ${shell}`, "warning");
		return;
	}
	const proceed = await ctx.ui.confirm(
		`Start a real ADW run for #${workItemId}?`,
		`This starts the ADW orchestrator (${shell}).\n\n` +
			"It performs REAL, mutating work: it creates and pushes a git branch, runs agents (model spend), " +
			"and opens a GitHub pull request \u2014 using git, gh, and the network. This is the only cockpit " +
			"command that changes state. Continue?",
	);
	if (!proceed) {
		ctx.ui.notify(`ADW run for #${workItemId} cancelled`, "info");
		return;
	}
	const summary = await runAndRecord(pi, ctx, `run #${workItemId}`, "npm", ["run", "issue", "--", workItemId], {
		cwd: adwPackageDir(ctx),
		timeout: 1_800_000,
	});
	notifyCommandResult(ctx, summary);
}

/** Command palette overlay. Resolves to an action id, or null on cancel. */
function pickMenuAction(ctx: ExtensionContext, footerOn: boolean): Promise<string | null> {
	return ctx.ui.custom<string | null>((tui, theme, _keybindings, done) => {
		const items: SelectItem[] = [
			{ value: "refresh", label: "Refresh cockpit", description: "re-read repo state (read-only)" },
			{ value: "runs", label: "Browse runs", description: "recent ADW runs; insert a resume command" },
			{ value: "config", label: "Inspect config", description: ".adw/config.json by section (read-only)" },
			{ value: "mvp", label: "MVP readiness", description: "PARITY / MVP-READINESS / HANDOVER (read-only)" },
			{ value: "check", label: "Run checks", description: "typecheck \u2192 lint:env \u2192 pack:check \u2192 test \u2192 build" },
			{ value: "dryrun", label: "Dry-run a work item", description: "preview the plan; no runner, no mutation" },
			{ value: "run", label: "Start a run  \u26a0", description: "REAL git/gh/network \u2014 branch + PR (guarded)" },
			{ value: "footer", label: footerOn ? "Hide status bar" : "Show status bar", description: "toggle the /adw-footer bar" },
		];
		const list = new SelectList(items, Math.min(items.length, 10), getSelectListTheme());
		list.onSelect = (item) => done(item.value);
		list.onCancel = () => done(null);
		const rule = (w: number) => theme.fg("border", BOX.h.repeat(Math.max(1, w)));
		return {
			invalidate: () => list.invalidate(),
			handleInput: (d: string) => {
				list.handleInput(d);
				tui.requestRender();
			},
			render: (width: number): string[] => {
				const w = Math.max(20, width);
				return [
					rule(w),
					`${theme.fg("accent", theme.bold("ADW MENU"))} ${theme.fg("dim", "\u00b7 command palette")}`,
					...list.render(w),
					rule(w),
					theme.fg("dim", "\u2191\u2193 navigate \u00b7 enter select \u00b7 esc cancel"),
				];
			},
		};
	});
}

// --- #issue autocomplete (ported from Pi's github-issue-autocomplete example) -

interface GitHubIssue {
	number: number;
	title: string;
	state: string;
}

const ISSUE_MAX = 100;
const ISSUE_MAX_SUGGESTIONS = 20;

function extractIssueToken(textBeforeCursor: string): string | undefined {
	const match = textBeforeCursor.match(/(?:^|[ \t])#([^\s#]*)$/);
	return match?.[1];
}

function parseGitHubRepo(remoteUrl: string): string | undefined {
	const ssh = remoteUrl.match(/^git@github\.com:([^/]+\/[^/]+?)(?:\.git)?$/);
	if (ssh) return ssh[1];
	const https = remoteUrl.match(/^https?:\/\/github\.com\/([^/]+\/[^/]+?)(?:\.git)?$/);
	return https ? https[1] : undefined;
}

async function resolveGitHubRepo(pi: ExtensionAPI, cwd: string): Promise<string | undefined> {
	try {
		const result = await pi.exec("git", ["remote", "-v"], { cwd, timeout: 5_000 });
		if (result.code !== 0) return undefined;
		for (const line of result.stdout.split("\n")) {
			const remoteUrl = line.trim().split(/\s+/)[1];
			if (!remoteUrl) continue;
			const repo = parseGitHubRepo(remoteUrl);
			if (repo) return repo;
		}
	} catch {
		return undefined;
	}
	return undefined;
}

function formatIssueItem(issue: GitHubIssue): AutocompleteItem {
	return { value: `#${issue.number}`, label: `#${issue.number}`, description: `[${issue.state.toLowerCase()}] ${issue.title}` };
}

function filterIssues(issues: GitHubIssue[], query: string): AutocompleteItem[] {
	if (!query.trim()) return issues.slice(0, ISSUE_MAX_SUGGESTIONS).map(formatIssueItem);
	if (/^\d+$/.test(query)) {
		const numeric = issues.filter((i) => String(i.number).startsWith(query)).slice(0, ISSUE_MAX_SUGGESTIONS).map(formatIssueItem);
		if (numeric.length > 0) return numeric;
	}
	return fuzzyFilter(issues, query, (i) => `${i.number} ${i.title}`).slice(0, ISSUE_MAX_SUGGESTIONS).map(formatIssueItem);
}

function createIssueAutocompleteProvider(
	current: AutocompleteProvider,
	getIssues: () => Promise<GitHubIssue[] | undefined>,
	isEnabled: () => boolean,
): AutocompleteProvider {
	return {
		async getSuggestions(lines, cursorLine, cursorCol, options): Promise<AutocompleteSuggestions | null> {
			const fallback = () => current.getSuggestions(lines, cursorLine, cursorCol, options);
			if (!isEnabled()) return fallback();
			const token = extractIssueToken((lines[cursorLine] ?? "").slice(0, cursorCol));
			if (token === undefined) return fallback();
			const issues = await getIssues();
			if (options.signal.aborted || !issues || issues.length === 0) return fallback();
			const suggestions = filterIssues(issues, token);
			return suggestions.length === 0 ? fallback() : { items: suggestions, prefix: `#${token}` };
		},
		applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
			return current.applyCompletion(lines, cursorLine, cursorCol, item, prefix);
		},
		shouldTriggerFileCompletion(lines, cursorLine, cursorCol) {
			return current.shouldTriggerFileCompletion?.(lines, cursorLine, cursorCol) ?? true;
		},
	};
}

/**
 * Register the #issue autocomplete provider for this session. It is lazy and
 * opt-in: no GitHub/network call happens until the provider is enabled (via
 * /adw-issues on) AND the user types a `#` token. Best-effort — a non-GitHub
 * repo or a `gh` failure simply yields no suggestions (no error spam).
 */
function setupIssueAutocomplete(pi: ExtensionAPI, ctx: ExtensionContext, isEnabled: () => boolean): void {
	let repoPromise: Promise<string | undefined> | undefined;
	let issuesPromise: Promise<GitHubIssue[] | undefined> | undefined;
	const getIssues = async (): Promise<GitHubIssue[] | undefined> => {
		repoPromise ||= resolveGitHubRepo(pi, ctx.cwd);
		const repo = await repoPromise;
		if (!repo) return undefined;
		issuesPromise ||= (async () => {
			try {
				const result = await pi.exec(
					"gh",
					["issue", "list", "--repo", repo, "--state", "open", "--limit", String(ISSUE_MAX), "--json", "number,title,state"],
					{ cwd: ctx.cwd, timeout: 5_000 },
				);
				if (result.code !== 0) return undefined;
				return JSON.parse(result.stdout) as GitHubIssue[];
			} catch {
				return undefined;
			}
		})();
		return issuesPromise;
	};
	ctx.ui.addAutocompleteProvider((current) => createIssueAutocompleteProvider(current, getIssues, isEnabled));
}

export default function adwCockpitExtension(pi: ExtensionAPI): void {
	let enabled = true;
	let footerEnabled = false;
	let issuesEnabled = false;
	const postCmd = (ctx: ExtensionContext) => {
		if (enabled) refresh(ctx);
		if (footerEnabled) applyFooter(ctx, true);
	};

	// Open the palette and dispatch the chosen action. Shared by the /adw-menu
	// command and the ctrl+shift+a shortcut so the two paths never drift.
	const openMenu = async (ctx: ExtensionContext): Promise<void> => {
		if (ctx.mode !== "tui") {
			ctx.ui.notify(
				"ADW: /adw /adw-refresh /adw-runs /adw-config /adw-mvp /adw-dry-run <id> /adw-check all /adw-run <id> /adw-footer /adw-issues",
				"info",
			);
			return;
		}
		await safely(ctx, "ADW menu", async () => {
			const action = await pickMenuAction(ctx, footerEnabled);
			if (!action) return;
			switch (action) {
				case "refresh":
					enabled = true;
					refresh(ctx);
					ctx.ui.notify("ADW Cockpit refreshed", "info");
					break;
				case "runs":
					await runsFlow(ctx);
					break;
				case "config":
					await configFlow(ctx);
					break;
				case "mvp":
					await mvpFlow(ctx);
					break;
				case "check":
					await execChecks(pi, ctx, "all");
					postCmd(ctx);
					break;
				case "dryrun": {
					const input = await ctx.ui.input("Dry-run which work item?", "issue number");
					const id = input ? parseWorkItemId(input) : null;
					if (!id) {
						ctx.ui.notify("Dry-run needs a numeric work-item id", "warning");
						break;
					}
					await execDryRun(pi, ctx, id);
					postCmd(ctx);
					break;
				}
				case "run": {
					const input = await ctx.ui.input("Start a REAL ADW run for which work item?", "issue number");
					const id = input ? parseWorkItemId(input) : null;
					if (!id) {
						ctx.ui.notify("A run needs a numeric work-item id", "warning");
						break;
					}
					await execGuardedRun(pi, ctx, id);
					postCmd(ctx);
					break;
				}
				case "footer":
					footerEnabled = !footerEnabled;
					applyFooter(ctx, footerEnabled);
					ctx.ui.notify(footerEnabled ? "ADW footer enabled" : "ADW footer restored to default", "info");
					break;
			}
		});
	};

	pi.on("session_start", (_event, ctx) => {
		if (enabled) refresh(ctx);
		if (footerEnabled) applyFooter(ctx, true);
		// Lazy, opt-in #issue autocomplete (TUI only; no network until enabled + '#').
		if (ctx.mode === "tui") setupIssueAutocomplete(pi, ctx, () => issuesEnabled);
	});

	pi.on("agent_end", (_event, ctx) => postCmd(ctx));

	// Release every UI surface this extension installs when the session ends.
	pi.on("session_shutdown", (_event, ctx) => {
		if (!ctx.hasUI) return;
		clear(ctx); // widgets + status
		ctx.ui.setFooter(undefined); // restore the built-in footer
	});

	pi.registerShortcut("ctrl+shift+a", {
		description: "Open the ADW menu",
		handler: (ctx) => openMenu(ctx),
	});

	pi.registerCommand("adw", {
		description: "Toggle the read-only ADW Cockpit widget (args: on/show/off/hide/toggle)",
		getArgumentCompletions: (prefix) => argCompletions(["on", "off", "toggle", "show", "hide"], prefix),
		handler: async (args, ctx) => {
			const action = args.trim().toLowerCase();
			if (action === "on" || action === "show") enabled = true;
			else if (action === "off" || action === "hide") enabled = false;
			else if (action === "" || action === "toggle") enabled = !enabled;
			else {
				ctx.ui.notify("Usage: /adw [on|show|off|hide|toggle]", "warning");
				return;
			}

			if (enabled) {
				refresh(ctx);
				ctx.ui.notify("ADW Cockpit shown", "info");
			} else {
				clear(ctx);
				ctx.ui.notify("ADW Cockpit hidden", "info");
			}
		},
	});

	pi.registerCommand("adw-refresh", {
		description: "Refresh the read-only ADW Cockpit widget",
		handler: async (_args, ctx) => {
			enabled = true;
			refresh(ctx);
			if (footerEnabled) applyFooter(ctx, true);
			ctx.ui.notify("ADW Cockpit refreshed", "info");
		},
	});

	pi.registerCommand("adw-footer", {
		description: "Toggle the compact ADW custom footer",
		getArgumentCompletions: (prefix) => argCompletions(["on", "off", "toggle", "show", "hide"], prefix),
		handler: async (args, ctx) => {
			const action = args.trim().toLowerCase();
			if (action === "on" || action === "show") footerEnabled = true;
			else if (action === "off" || action === "hide") footerEnabled = false;
			else if (action === "" || action === "toggle") footerEnabled = !footerEnabled;
			else {
				ctx.ui.notify("Usage: /adw-footer [on|show|off|hide|toggle]", "warning");
				return;
			}
			applyFooter(ctx, footerEnabled);
			ctx.ui.notify(footerEnabled ? "ADW footer enabled" : "ADW footer restored to default", "info");
		},
	});

	pi.registerCommand("adw-menu", {
		description: "Open the ADW command palette (also: ctrl+shift+a)",
		handler: async (_args, ctx) => openMenu(ctx),
	});

	pi.registerCommand("adw-dry-run", {
		description: "Run an ADW CLI dry-run for a work item (read-only preview; does not invoke a runner)",
		handler: async (args, ctx) => {
			const workItemId = parseWorkItemId(args);
			if (!workItemId) {
				ctx.ui.notify("Usage: /adw-dry-run <work-item-id>", "warning");
				return;
			}
			await execDryRun(pi, ctx, workItemId);
			postCmd(ctx);
		},
	});

	pi.registerCommand("adw-run", {
		description: "Start a REAL, guarded ADW run for a work item (mutates git/forge; asks to confirm)",
		handler: async (args, ctx) => {
			const workItemId = parseWorkItemId(args);
			if (!workItemId) {
				ctx.ui.notify("Usage: /adw-run <work-item-id>", "warning");
				return;
			}
			await execGuardedRun(pi, ctx, workItemId);
			postCmd(ctx);
		},
	});

	pi.registerCommand("adw-check", {
		description: "Run an explicit ADW package check: typecheck | lint-env | pack-check | test | build | all",
		getArgumentCompletions: (prefix) => argCompletions(["typecheck", "lint-env", "pack-check", "test", "build", "all"], prefix),
		handler: async (args, ctx) => {
			await execChecks(pi, ctx, args.trim() || "typecheck");
			postCmd(ctx);
		},
	});

	pi.registerCommand("adw-config", {
		description: "Inspect .adw/config.json by section (read-only overlay)",
		handler: async (_args, ctx) => {
			await configFlow(ctx);
		},
	});

	pi.registerCommand("adw-mvp", {
		description: "Summarize MVP-readiness docs (PARITY/MVP-READINESS/HANDOVER; read-only)",
		handler: async (_args, ctx) => {
			await mvpFlow(ctx);
		},
	});

	pi.registerCommand("adw-runs", {
		description: "Browse recent ADW runs (read-only); can insert a resume command into the editor",
		handler: async (_args, ctx) => {
			await runsFlow(ctx);
		},
	});

	pi.registerCommand("adw-issues", {
		description: "Toggle #issue autocomplete (uses gh + the network when on)",
		getArgumentCompletions: (prefix) => argCompletions(["on", "off", "toggle"], prefix),
		handler: async (args, ctx) => {
			const action = args.trim().toLowerCase();
			if (action === "on" || action === "show") issuesEnabled = true;
			else if (action === "off" || action === "hide") issuesEnabled = false;
			else if (action === "" || action === "toggle") issuesEnabled = !issuesEnabled;
			else {
				ctx.ui.notify("Usage: /adw-issues [on|off|toggle]", "warning");
				return;
			}
			ctx.ui.notify(
				issuesEnabled
					? "Issue autocomplete on \u2014 typing '#' queries open GitHub issues via gh (network)."
					: "Issue autocomplete off.",
				"info",
			);
		},
	});
}
