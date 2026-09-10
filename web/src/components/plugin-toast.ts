// A plugin UI used to announce itself by appearing in the sidebar. Pins replaced that
// (design/web-console-overhaul.md §4.1), so a fresh install now says where it went once.
//
// Driven by the plugin directory rather than by the install job: a plugin installed from the
// CLI, or from another browser tab, deserves the same line, and the directory is where "a
// plugin with a UI exists now" is actually true.

import { toast } from "@unom/ui/toast";
import { useEffect } from "react";
import { uiPlugins, usePlugins } from "@/api/plugins";
import { isStringArray, useLocalPref } from "@/lib/prefs";
import { m } from "@/paraglide/messages";

/**
 * Absent means this browser has never looked, which is NOT the same as "no plugins": on a
 * first visit to a host that already has one, every id is new. So the first observation seeds
 * silently and only later arrivals are announced.
 */
const isSeen = (v: unknown): v is string[] | null =>
	v === null || isStringArray(v);
const UNSEEDED: string[] | null = null;

export function useNewPluginToast(): void {
	const { data } = usePlugins();
	const [seen, setSeen] = useLocalPref("pf-plugins-seen", UNSEEDED, isSeen);

	useEffect(() => {
		if (!data) return;
		const ids = uiPlugins(data).map((p) => p.id);
		if (seen === null) {
			setSeen(ids);
			return;
		}
		const fresh = uiPlugins(data).filter((p) => !seen.includes(p.id));
		// Write only on a real change, or the effect re-runs on its own notification forever.
		const same =
			ids.length === seen.length && ids.every((id) => seen.includes(id));
		if (!same) setSeen(ids);
		for (const p of fresh) {
			toast.success(m.plugin_installed_toast({ name: p.title }));
		}
	}, [data, seen, setSeen]);
}
