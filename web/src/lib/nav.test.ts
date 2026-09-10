import { describe, expect, test } from "bun:test";
import {
	MANAGE,
	NAV,
	type PinnablePlugin,
	PRIMARY,
	pluginPin,
	resolvePins,
	togglePin,
} from "./nav";

const plugin = (id: string): PinnablePlugin => ({ id, title: id });

describe("nav table", () => {
	test("five primary destinations, the rest under Manage", () => {
		expect(PRIMARY).toHaveLength(5);
		expect(PRIMARY.map((n) => n.to)).toEqual([
			"/",
			"/pairing",
			"/displays",
			"/library",
			"/host",
		]);
		expect(PRIMARY.length + MANAGE.length).toBe(NAV.length);
	});

	// "/" matches every path as a prefix, and "/plugins" sits above "/plugins/<id>" — a plugin's
	// own page — so both would light up alongside the page you are actually on.
	test("only the prefix-ambiguous routes are exact", () => {
		expect(NAV.filter((n) => n.exact).map((n) => n.to)).toEqual([
			"/",
			"/plugins",
		]);
	});

	test("every destination carries a phone-list hint", () => {
		for (const n of NAV) expect(n.hint()).not.toBe("");
	});
});

describe("resolvePins", () => {
	test("keeps the operator's order across both kinds", () => {
		const resolved = resolvePins(
			[pluginPin("rom-manager"), "/logs"],
			[plugin("rom-manager")],
		);
		expect(resolved.map((p) => p.kind)).toEqual(["plugin", "nav"]);
		expect(resolved[0]).toMatchObject({ plugin: { id: "rom-manager" } });
		expect(resolved[1]).toMatchObject({ entry: { to: "/logs" } });
	});

	// The pin outlives the plugin on purpose: uninstall/reinstall must not silently lose it.
	test("drops a pin nothing resolves, without touching its neighbours", () => {
		const resolved = resolvePins(
			["/logs", pluginPin("since-removed"), "/stats"],
			[],
		);
		expect(resolved.map((p) => p.kind === "nav" && p.entry.to)).toEqual([
			"/logs",
			"/stats",
		]);
	});

	// A plugin id is not a route. Without the prefix a plugin called "logs" would resolve to
	// Troubleshooting, and a raw route id would match a plugin of the same name.
	test("a plugin id does not resolve as a route", () => {
		expect(resolvePins(["logs"], [plugin("logs")])).toEqual([]);
		expect(resolvePins([pluginPin("logs")], [plugin("logs")])).toHaveLength(1);
	});

	test("a primary destination cannot be pinned — it is already there", () => {
		expect(resolvePins(["/host"], [])).toEqual([]);
	});
});

describe("togglePin", () => {
	test("appends, then removes, leaving order otherwise intact", () => {
		expect(togglePin(["/logs"], "/stats")).toEqual(["/logs", "/stats"]);
		expect(togglePin(["/logs", "/stats"], "/logs")).toEqual(["/stats"]);
	});
});
