import { useState } from "react";
import { ArrowRight, Download, HardDrive, Server, ShieldCheck } from "lucide-react";

import { AppDialog } from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { pullModel, reportFailure } from "@/lib/actions";
import { api } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { queryClient } from "@/lib/query-client";
import {
  hardwareSummary,
  pickEntries,
  queryKeys,
  useCatalog,
  useDoctor,
  useHardware,
  useSettingsQuery,
} from "@/lib/queries";
import { useUi } from "@/stores/ui";

const STEPS = ["What stays on this machine", "What this machine can run", "Pick a first model"];

/**
 * The first-run screen `Settings.onboardingComplete` has always named. It is
 * three questions the app can already answer — what leaves the machine, what
 * this hardware can hold, and which model to download first — asked once,
 * before the eight pages make any sense.
 *
 * It never blocks a returning user: the whole component waits for settings to
 * load, and an unreadable settings file leaves the flag false, which means the
 * screen shows again rather than hiding a first-run step.
 */
export function Onboarding() {
  const settings = useSettingsQuery();
  const current = settings.data;
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);

  if (!current || current.onboardingComplete) {
    return null;
  }

  const finish = async (then?: () => void) => {
    setBusy(true);
    try {
      await api.saveSettings({ ...current, onboardingComplete: true });
      await queryClient.invalidateQueries({ queryKey: queryKeys.settings });
      then?.();
    } catch (error) {
      // This screen writes one boolean. If even that fails, the toast is how you
      // learn the question will be asked again next launch.
      reportFailure("Could not finish onboarding", error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppDialog
      open
      // Dismissing without answering is its own answer: the flag is set either
      // way, so a returning user is never asked twice.
      onOpenChange={(open) => {
        if (!open && !busy) void finish();
      }}
      title={STEPS[step]}
      description="Three steps, once. Everything here is a decision you can change in Settings."
      className="max-w-xl"
      footer={
        <div className="flex w-full items-center justify-between gap-3">
          <div className="flex gap-1.5">
            {STEPS.map((label, index) => (
              <span
                key={label}
                aria-hidden
                className={
                  index === step
                    ? "h-1.5 w-6 rounded-full bg-primary"
                    : "h-1.5 w-3 rounded-full bg-border"
                }
              />
            ))}
          </div>
          <div className="flex gap-2">
            {step < STEPS.length - 1 ? (
              <>
                <Button variant="ghost" size="sm" onClick={() => void finish()} disabled={busy}>
                  Skip
                </Button>
                <Button size="sm" onClick={() => setStep(step + 1)}>
                  Next
                  <ArrowRight className="h-3.5 w-3.5" />
                </Button>
              </>
            ) : (
              // The last step's exit is not a deferral, so it does not say "Later"
              // — a user who has read three steps means "I am done".
              <Button size="sm" variant="ghost" onClick={() => void finish()} disabled={busy}>
                Done
              </Button>
            )}
          </div>
        </div>
      }
    >
      {step === 0 ? <LocalStep /> : null}
      {step === 1 ? <MachineStep /> : null}
      {step === 2 ? (
        <ModelStep busy={busy} onDone={() => void finish(() => useUi.getState().setPage("models"))} />
      ) : null}
    </AppDialog>
  );
}

function LocalStep() {
  return (
    <ul className="space-y-3 text-sm leading-relaxed">
      {[
        {
          icon: ShieldCheck,
          title: "Nothing is uploaded, ever",
          body: "No account, no analytics, no update pinger. The only request the app makes is the model download you start yourself, and it goes to the registry you chose.",
        },
        {
          icon: HardDrive,
          title: "Models live in a folder you can see",
          body: "Weights, transcripts and settings are plain files under the app's data folder. Settings can move the model folder to another disk with its files.",
        },
        {
          icon: Server,
          title: "The server stays on loopback",
          body: "The OpenAI-compatible API listens on 127.0.0.1 so other programs on this machine can use your model. Binding anywhere else is refused.",
        },
      ].map((row) => (
        <li key={row.title} className="flex gap-3">
          <row.icon className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
          <span>
            <span className="block font-medium text-foreground">{row.title}</span>
            <span className="text-muted-foreground">{row.body}</span>
          </span>
        </li>
      ))}
    </ul>
  );
}

function MachineStep() {
  const hardware = useHardware();
  const doctor = useDoctor();
  if (hardware.isPending || doctor.isPending) {
    return <p className="text-sm text-muted-foreground">Measuring this machine…</p>;
  }
  if (hardware.isError || !hardware.data) {
    return (
      <p className="text-sm text-muted-foreground">
        Hardware detection failed, so this step is empty rather than invented. The rest of the app
        still works; Settings can retry.
      </p>
    );
  }

  const report = doctor.data;
  return (
    <div className="space-y-4 text-sm">
      <p className="font-medium text-foreground">{report?.headline || hardwareSummary(hardware.data)}</p>
      <dl className="grid grid-cols-2 gap-3">
        <Fact label="Processor" value={hardware.data.cpuBrand || hardware.data.platform} />
        <Fact label="Cores" value={`${hardware.data.logicalCores} logical`} />
        <Fact label="Memory" value={`${hardware.data.totalRamGb.toFixed(0)} GB`} />
        <Fact
          label="Recommended backend"
          value={report?.backendRecommendation ?? hardware.data.platform}
        />
      </dl>
      {report && report.warnings.length > 0 ? (
        <ul className="space-y-1.5">
          {report.warnings.map((warning) => (
            <li key={warning} className="flex gap-2 text-xs text-muted-foreground">
              <span className="text-amber-500 dark:text-amber-400" aria-hidden>
                •
              </span>
              {warning}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-xs text-muted-foreground">
          The doctor has no warnings about this machine, so nothing rules the catalog's models out
          on hardware grounds.
        </p>
      )}
    </div>
  );
}

function ModelStep({ onDone, busy }: { onDone: () => void; busy: boolean }) {
  const doctor = useDoctor();
  const catalog = useCatalog({ query: "", sort: "smallest", hidePlaceholders: true });
  const suggested = pickEntries(
    catalog.data ?? [],
    (doctor.data?.recommendedModels ?? []).slice(0, 3),
  );

  if (suggested.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        The catalog is not reachable right now. Open the Models page whenever you are ready to
        pick one.
      </p>
    );
  }

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        These are the ones this machine can actually hold. A download resumes if it drops, and you
        can delete a model at any time.
      </p>
      {suggested.map((entry) => (
        <div
          key={entry.id}
          className="flex items-center justify-between gap-3 rounded-lg border p-3"
        >
          <div className="min-w-0">
            <p className="truncate text-sm font-medium">{entry.displayName}</p>
            <p className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
              <span>{entry.parametersB}B · {entry.quantization}</span>
              <Badge tone={entry.fitsMemory ? "success" : "warning"}>
                {formatBytes(entry.sizeMb * 1024 * 1024)}
              </Badge>
            </p>
          </div>
          <Button
            size="sm"
            disabled={busy || entry.downloaded || entry.downloading}
            onClick={() =>
              // Only leave onboarding if the transfer actually started; a failure
              // stays on this card with its toast, so the button is still there.
              void pullModel(entry.id).then((task) => {
                if (task) {
                  onDone();
                }
              })
            }
          >
            <Download className="h-3.5 w-3.5" />
            {entry.downloaded ? "Downloaded" : "Download"}
          </Button>
        </div>
      ))}
    </div>
  );
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs uppercase tracking-wide text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 truncate text-sm font-medium" title={value}>
        {value}
      </dd>
    </div>
  );
}
