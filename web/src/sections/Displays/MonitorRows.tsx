// **Your monitors** — the host's real heads, one row each (design §5.1, §5.4).
//
// This absorbs the old Streamed screen card: the choice of streaming a real monitor instead of
// a virtual one is a property OF a monitor, so the radio sits on the monitor's row rather than
// in a separate card with its own 214-character introduction.
import type { FC } from "react";
import type { ApiMonitorInfo, DisplayPolicy } from "@/api/gen/model";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { m } from "@/paraglide/messages";

export const MonitorRows: FC<{
	monitors: readonly ApiMonitorInfo[];
	/** The pin the host actually has, env override included. */
	pinned: string | null;
	/** The host can honour a pin at all (`enforced` carries `capture_monitor`). */
	pinSupported: boolean;
	policy?: DisplayPolicy;
	busy?: boolean;
	onPick: (connector: string | null) => void;
}> = ({ monitors, pinned, pinSupported, policy, busy, onPick }) => {
	// Our own virtual displays show up in the head list on KWin; they are on the map already and
	// are not something to stream FROM.
	const heads = monitors.filter((mon) => !mon.managed);
	if (heads.length === 0) return null;
	// `PUNKTFUNK_CAPTURE_MONITOR` outranks the stored policy, so a host pinned in its unit's
	// environment is read-only: offering controls that silently lose to the env is worse than
	// saying nothing.
	const envLocked = !!pinned && !!policy && policy.capture_monitor !== pinned;
	const locked = busy || envLocked;

	return (
		<Card>
			<CardContent className="space-y-3">
				<h2 className="text-sm font-medium">{m.display_your_monitors()}</h2>
				{envLocked && (
					<p className="text-sm text-amber-600 dark:text-amber-500">
						{m.display_monitor_env_locked()}
					</p>
				)}
				<ul className="divide-y rounded-md border">
					{pinSupported && (
						<Row
							selected={!pinned}
							disabled={locked}
							title={m.display_stream_virtual()}
							onPick={() => onPick(null)}
						/>
					)}
					{heads.map((mon) => (
						<Row
							key={mon.connector}
							selected={pinned?.toLowerCase() === mon.connector.toLowerCase()}
							// A disabled head cannot be streamed — the host refuses with that reason —
							// but it stays listed so "why isn't my monitor here?" has an answer.
							disabled={locked || !mon.enabled}
							title={`${mon.connector} — ${mon.description}`}
							detail={mon.mode}
							badges={
								<>
									{mon.primary && (
										<Badge variant="secondary">
											{m.display_monitor_primary()}
										</Badge>
									)}
									{!mon.enabled && (
										<Badge variant="outline">
											{m.display_monitor_disabled()}
										</Badge>
									)}
								</>
							}
							// No radio at all where the host cannot honour a pin: the row is then
							// just the inventory it always was.
							onPick={pinSupported ? () => onPick(mon.connector) : undefined}
						/>
					))}
				</ul>
			</CardContent>
		</Card>
	);
};

const Row: FC<{
	selected: boolean;
	disabled: boolean;
	title: string;
	detail?: string;
	badges?: React.ReactNode;
	onPick?: () => void;
}> = ({ selected, disabled, title, detail, badges, onPick }) => {
	const body = (
		<>
			<span className="min-w-0 flex-1">
				<span className="flex flex-wrap items-center gap-2 font-medium">
					{title}
					{badges}
				</span>
				{detail && (
					<span className="block text-sm text-muted-foreground">{detail}</span>
				)}
			</span>
			{onPick && (
				<span className="text-sm text-muted-foreground">
					{selected ? m.display_map_streamed() : m.display_stream_this()}
				</span>
			)}
		</>
	);
	if (!onPick) {
		return <li className="flex items-center gap-3 px-3 py-2">{body}</li>;
	}
	return (
		<li>
			<button
				type="button"
				disabled={disabled}
				aria-pressed={selected}
				onClick={onPick}
				className={cn(
					"flex w-full items-center gap-3 px-3 py-2 text-left transition-colors",
					selected ? "bg-primary/10" : "hover:bg-muted/50",
					disabled && "cursor-not-allowed opacity-60",
				)}
			>
				{body}
			</button>
		</li>
	);
};
