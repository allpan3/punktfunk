// The two disclosures at the foot of the Displays page (design §5.1).
//
// `<details>` rather than a state-driven accordion: the browser already ships the open/close,
// the keyboard handling and the aria wiring, and a closed section costs one row of height
// instead of the card each of these used to own.
import type { FC, ReactNode } from "react";
import type { DisplayPolicy, GameSession } from "@/api/gen/model";
import { usePlatform } from "@/api/platform";
import { DocsLink } from "@/components/docs-link";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { m } from "@/paraglide/messages";
import { SessionGameCard } from "./SessionGameCard";

const Disclosure: FC<{ label: string; children: ReactNode }> = ({
	label,
	children,
}) => (
	<Card>
		<CardContent flush className="p-0">
			<details className="group">
				<summary className="cursor-pointer list-none px-4 py-3 text-sm font-medium marker:content-none">
					<span className="inline-block transition-transform group-open:rotate-90">
						▸
					</span>{" "}
					{label}
				</summary>
				<div className="space-y-5 border-t px-4 py-4">{children}</div>
			</details>
		</CardContent>
	</Card>
);

/** Two-value toggle rows share this shape; each writes one field and saves on change. */
const Toggle: FC<{
	label: string;
	help: string;
	docs: string;
	value: boolean;
	busy?: boolean;
	onSet: (on: boolean) => void;
}> = ({ label, help, docs, value, busy, onSet }) => (
	<fieldset className="space-y-2">
		<legend className="text-sm font-medium">{label}</legend>
		<div className="flex flex-wrap gap-2">
			{([false, true] as const).map((on) => (
				<Button
					key={String(on)}
					size="sm"
					variant={value === on ? "default" : "outline"}
					aria-pressed={value === on}
					disabled={busy}
					onClick={() => onSet(on)}
				>
					{on ? m.common_on() : m.common_off()}
				</Button>
			))}
		</div>
		<p className="max-w-prose text-xs text-muted-foreground">
			{help} <DocsLink path={docs} />
		</p>
	</fieldset>
);

/**
 * Dedicated game sessions, plus the session⇄game lifetime it interacts with: keep-alive decides
 * how long a *display* outlives a disconnect, these decide whether the *game* does.
 */
export const GameSessionDisclosure: FC<{
	policy?: DisplayPolicy;
	busy?: boolean;
	onSet: (patch: Partial<DisplayPolicy>) => void;
}> = ({ policy, busy, onSet }) => {
	const { acts, actsAny } = usePlatform();
	const axis = acts("display", "game_session");
	// Nothing to disclose: this host neither routes launches nor ties a game to its session.
	if (!axis && !actsAny("session")) return null;
	return (
		<Disclosure label={m.display_game_session()}>
			{axis && (
				<fieldset className="space-y-2">
					<legend className="text-sm font-medium">
						{m.display_game_session()}
					</legend>
					<div className="flex flex-wrap gap-2">
						{(["auto", "dedicated"] as const).map((v) => (
							<Button
								key={v}
								size="sm"
								variant={
									(policy?.game_session ?? "auto") === v ? "default" : "outline"
								}
								aria-pressed={(policy?.game_session ?? "auto") === v}
								disabled={busy}
								onClick={() => onSet({ game_session: v as GameSession })}
							>
								{v === "auto"
									? m.display_game_session_auto()
									: m.display_game_session_dedicated()}
							</Button>
						))}
					</div>
					<p className="max-w-prose text-xs text-muted-foreground">
						{m.display_game_session_help()}{" "}
						<DocsLink path="virtual-displays#dedicated-game-sessions" />
					</p>
				</fieldset>
			)}
			<SessionGameCard />
		</Disclosure>
	);
};

/** The Windows exclusive-isolate levers. Rendered only where the host acts on one (D1). */
export const AdvancedDisclosure: FC<{
	policy?: DisplayPolicy;
	busy?: boolean;
	onSet: (patch: Partial<DisplayPolicy>) => void;
}> = ({ policy, busy, onSet }) => {
	const { acts } = usePlatform();
	const fields = (
		[
			[
				"ddc_power_off",
				m.display_ddc(),
				m.display_ddc_help(),
				"virtual-displays#power-monitors-off-ddcci",
			],
			[
				"pnp_disable_monitors",
				m.display_pnp(),
				m.display_pnp_help(),
				"virtual-displays#disable-monitor-devices-pnp",
			],
			[
				"edid_lock",
				m.display_edid(),
				m.display_edid_help(),
				"virtual-displays#hold-monitor-identity-edid",
			],
		] as const
	).filter(([field]) => acts("display", field));
	if (fields.length === 0) return null;
	return (
		<Disclosure label={m.display_advanced()}>
			{fields.map(([field, label, help, docs]) => (
				<Toggle
					key={field}
					label={label}
					help={help}
					docs={docs}
					value={policy?.[field] ?? false}
					busy={busy}
					onSet={(on) => onSet({ [field]: on })}
				/>
			))}
		</Disclosure>
	);
};
