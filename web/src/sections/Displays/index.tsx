// The **Displays** page (design/web-console-overhaul.md §5).
//
// State above configuration, no tabs: the map and the device rows say what is happening, the
// sentence says what will happen next, and [ Change ] is the one way to change it. The five-
// second test this page has to pass is "what happens to my monitors when a device connects" —
// the map and the sentence ARE the answer, which is why the help text under every control
// could go.
//
// One persistence model: everything saves on change. The host applies at the next connect
// either way, so there was never anything for a Save button to protect.
import { useQueryClient } from "@tanstack/react-query";
import Section from "@unom/ui/section";
import { toast } from "@unom/ui/toast";
import { type FC, useState } from "react";
import {
	getGetDisplayMonitorsQueryKey,
	getGetDisplaySettingsQueryKey,
	getGetDisplayStateQueryKey,
	useCreateCustomPreset,
	useDeleteCustomPreset,
	useGetDisplayMonitors,
	useGetDisplaySettings,
	useGetDisplayState,
	useReleaseDisplay,
	useSetDisplayLayout,
	useSetDisplaySettings,
	useUpdateCustomPreset,
} from "@/api/gen/display/display";
import type {
	CustomPreset,
	DisplayPolicy,
	EffectivePolicy,
} from "@/api/gen/model";
import { usePlatform } from "@/api/platform";
import { useDialogs } from "@/components/dialogs";
import { DocsLink } from "@/components/docs-link";
import { QueryState } from "@/components/query-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { apiErrorMessage } from "@/lib/errors";
import { useLocale } from "@/lib/i18n";
import { m } from "@/paraglide/messages";
import { BehaviourSheet } from "./BehaviourSheet";
import { DesktopMap } from "./DesktopMap";
import { AdvancedDisclosure, GameSessionDisclosure } from "./Disclosures";
import { describePolicy } from "./describePolicy";
import { MonitorRows } from "./MonitorRows";

export const SectionDisplays: FC = () => {
	useLocale();
	const qc = useQueryClient();
	const { confirm, promptText } = useDialogs();
	const { acts } = usePlatform();

	const settings = useGetDisplaySettings();
	const monitors = useGetDisplayMonitors();
	// Create/release arrive on the event stream, so the timer only covers the one thing events
	// cannot express: the per-second countdown on a kept display.
	const state = useGetDisplayState({
		query: {
			refetchInterval: (q) =>
				q.state.data?.displays?.some((d) => d.expires_in_ms != null)
					? 2_000
					: 15_000,
		},
	});
	const save = useSetDisplaySettings();
	const release = useReleaseDisplay();
	const saveLayout = useSetDisplayLayout();
	const createPreset = useCreateCustomPreset();
	const updatePreset = useUpdateCustomPreset();
	const deletePreset = useDeleteCustomPreset();

	const [sheetOpen, setSheetOpen] = useState(false);
	// What the map should show while a preset card is hovered — never written anywhere.
	const [preview, setPreview] = useState<EffectivePolicy | undefined>();

	const policy = settings.data?.settings;
	const effective = settings.data?.effective;
	const displays = state.data?.displays ?? [];
	const heads = monitors.data?.monitors ?? [];
	const live = displays.some((d) => d.state === "active");
	const kept = displays.filter((d) => d.state !== "active");
	const shown = preview ?? effective;
	const busy = save.isPending;

	const invalidate = () => {
		qc.invalidateQueries({ queryKey: getGetDisplaySettingsQueryKey() });
		qc.invalidateQueries({ queryKey: getGetDisplayMonitorsQueryKey() });
	};

	/** The page's only policy write: the stored policy with some fields replaced. */
	const write = (patch: Partial<DisplayPolicy>) => {
		if (!policy) return;
		save.mutate(
			// `capture_monitor` is read back from the server on every write rather than carried
			// through a component: the monitor rows own it, and a stale copy here is exactly how
			// applying a preset used to un-pin a mirroring host.
			{ data: { ...policy, ...patch } },
			{
				onSuccess: () => {
					invalidate();
					toast.success(m.display_settings_saved());
				},
			},
		);
	};

	const doRelease = (slot?: number) =>
		release.mutate(
			{ data: { slot: slot ?? null } },
			{
				onSuccess: () =>
					qc.invalidateQueries({ queryKey: getGetDisplayStateQueryKey() }),
			},
		);

	/** Drag-drop on the map: place one screen and switch the host to a manual layout. */
	const moveDisplay = (slot: number, x: number, y: number) => {
		const d = displays.find((it) => it.slot === slot);
		if (d?.identity_slot == null) return;
		// `PUT /display/layout` REPLACES the whole map, so every stored position has to ride
		// along or an offline device's saved placement is deleted.
		const positions = { ...(policy?.layout?.positions ?? {}) };
		positions[String(d.identity_slot)] = { x, y };
		saveLayout.mutate(
			{ data: { positions } },
			{
				onSuccess: () => {
					invalidate();
					qc.invalidateQueries({ queryKey: getGetDisplayStateQueryKey() });
				},
			},
		);
	};

	const savePreset = async () => {
		if (!effective) return;
		const name = (
			await promptText({
				title: m.display_preset_save_title(),
				label: m.display_preset_name(),
			})
		)?.trim();
		if (!name) return;
		createPreset.mutate(
			{
				data: {
					name,
					fields: effective,
					game_session: policy?.game_session ?? "auto",
				},
			},
			{ onSuccess: invalidate },
		);
	};

	const renamePreset = async (p: CustomPreset) => {
		const name = (
			await promptText({
				title: m.display_preset_edit(),
				label: m.display_preset_name(),
				defaultValue: p.name,
			})
		)?.trim();
		if (!name) return;
		updatePreset.mutate(
			{
				id: p.id,
				data: {
					name,
					fields: p.fields,
					game_session: p.game_session ?? "auto",
				},
			},
			{ onSuccess: invalidate },
		);
	};

	const removePreset = async (p: CustomPreset) => {
		const ok = await confirm({
			title: m.display_preset_delete_confirm(),
			confirmLabel: m.display_preset_delete(),
			destructive: true,
		});
		if (ok) deletePreset.mutate({ id: p.id }, { onSuccess: invalidate });
	};

	const error = apiErrorMessage(save.error ?? saveLayout.error);

	return (
		<Section maxWidth={false}>
			<div className="flex flex-col gap-card">
				<div className="flex flex-wrap items-center gap-3">
					<h1 className="text-2xl font-semibold">{m.nav_displays()}</h1>
					<DocsLink path="virtual-displays" className="text-sm" />
					{kept.length > 0 && (
						<Button
							size="sm"
							variant="outline"
							className="ml-auto"
							disabled={release.isPending}
							onClick={() => doRelease()}
						>
							{m.display_release_all()}
						</Button>
					)}
				</div>

				<Card>
					<CardContent className="space-y-4">
						<QueryState
							isLoading={settings.isLoading || monitors.isLoading}
							error={effective ? undefined : (settings.error ?? monitors.error)}
							refetch={settings.refetch}
						>
							{heads.length + displays.length === 0 ? (
								<p className="text-sm text-muted-foreground">
									{m.display_map_empty()}
								</p>
							) : (
								<DesktopMap
									monitors={heads}
									displays={displays}
									dimMonitors={shown?.topology === "exclusive"}
									captureMonitor={monitors.data?.pinned ?? null}
									onRelease={doRelease}
									onMove={moveDisplay}
									busy={release.isPending || saveLayout.isPending}
								/>
							)}
							{shown && (
								<div className="flex flex-wrap items-start gap-3">
									<p className="min-w-0 flex-1 text-sm">
										{/* Read from the host's `effective`, never a local draft — the old
										    badge row restated the operator's unsaved edits back to them
										    as though the host had already adopted them. */}
										{describePolicy(shown, { live })}
										{shown.layout.mode === "manual" && (
											<> {m.display_arranged_by_you()}</>
										)}
									</p>
									<Button
										size="sm"
										disabled={busy || !policy}
										onClick={() => setSheetOpen(true)}
									>
										{m.display_change()}
									</Button>
								</div>
							)}
							{displays.length > 1 && (
								<p className="text-xs text-muted-foreground">
									{m.display_arrange_hint()}
								</p>
							)}
							{error && <p className="text-sm text-destructive">{error}</p>}
						</QueryState>
					</CardContent>
				</Card>

				{/* The rows are the map in words: the keyboard and screen-reader path, and what a
				    phone falls back to when a box would be under 44 px. */}
				<Card>
					<CardContent className="space-y-4">
						<h2 className="text-sm font-medium">{m.display_devices()}</h2>
						{displays.length === 0 ? (
							<p className="text-sm text-muted-foreground">
								{m.display_no_devices()}
							</p>
						) : (
							<ul className="divide-y rounded-md border">
								{displays.map((d) => (
									<li
										key={d.slot}
										className="flex flex-wrap items-center gap-3 px-3 py-2 text-sm"
									>
										<span className="min-w-0 flex-1 truncate font-medium">
											{d.client ?? m.display_map_unnamed()}
										</span>
										<span className="text-muted-foreground">{d.mode}</span>
										<Badge
											variant={d.state === "active" ? "default" : "outline"}
										>
											{d.state === "active"
												? m.display_state_streaming()
												: d.state === "pinned"
													? m.display_state_kept_until()
													: m.display_state_kept()}
										</Badge>
										{d.state !== "active" && (
											<Button
												size="sm"
												variant="ghost"
												disabled={release.isPending}
												onClick={() => doRelease(d.slot)}
											>
												{m.display_release()}
											</Button>
										)}
									</li>
								))}
							</ul>
						)}
					</CardContent>
				</Card>

				<MonitorRows
					monitors={heads}
					pinned={monitors.data?.pinned ?? null}
					pinSupported={acts("display", "capture_monitor")}
					policy={policy}
					busy={busy}
					onPick={(connector) => write({ capture_monitor: connector })}
				/>

				<GameSessionDisclosure policy={policy} busy={busy} onSet={write} />
				<AdvancedDisclosure policy={policy} busy={busy} onSet={write} />
			</div>

			{policy && effective && settings.data && (
				<BehaviourSheet
					open={sheetOpen}
					onOpenChange={setSheetOpen}
					effective={effective}
					policy={policy}
					presets={settings.data.presets}
					customPresets={settings.data.custom_presets}
					onApply={(p) => write(p)}
					onSetField={write}
					onSavePreset={savePreset}
					onRenamePreset={renamePreset}
					onUpdatePreset={(p) =>
						updatePreset.mutate(
							{
								id: p.id,
								data: {
									name: p.name,
									fields: effective,
									game_session: policy.game_session ?? "auto",
								},
							},
							{ onSuccess: invalidate },
						)
					}
					onDeletePreset={removePreset}
					busy={busy}
					onPreview={setPreview}
				/>
			)}
		</Section>
	);
};
