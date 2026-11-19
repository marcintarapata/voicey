import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, CircleAlert, Download, RotateCw } from "lucide-react";
import { commands, type PermissionAccess } from "@/bindings";
import { useModelStore } from "@/stores/modelStore";
import { useOsType } from "@/hooks/useOsType";
import { PINNED_MODEL_ID } from "@/lib/pinnedModel";
import { ProgressBar } from "@/components/shared";
import { Button } from "@/components/ui/Button";
import { SettingsGroup } from "@/components/ui/SettingsGroup";
import { Spinner } from "@/components/ui/Spinner";

/**
 * First-run setup, in Voicey's own style: the microphone status and the
 * pinned Parakeet download with its progress — nothing else is asked of the
 * user. The backend starts the download automatically at startup; this panel
 * just tracks it (and offers a retry when it fails). Once Parakeet is on disk
 * the panel disappears and the two-setting surface takes over; a blocked
 * microphone after setup surfaces through the dictation notices instead.
 */
export const FirstRunSetup: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const isWindows = osType === "windows";

  const models = useModelStore((state) => state.models);
  const modelsLoading = useModelStore((state) => state.loading);
  const downloadingModels = useModelStore((state) => state.downloadingModels);
  const verifyingModels = useModelStore((state) => state.verifyingModels);
  const downloadProgress = useModelStore((state) => state.downloadProgress);
  const downloadError = useModelStore((state) => state.error);
  const downloadModel = useModelStore((state) => state.downloadModel);

  const parakeet = models.find((model) => model.id === PINNED_MODEL_ID);
  const modelDownloaded = parakeet?.is_downloaded ?? false;
  const modelDownloading =
    (parakeet?.is_downloading ?? false) || PINNED_MODEL_ID in downloadingModels;
  const modelVerifying = PINNED_MODEL_ID in verifyingModels;
  const progress = downloadProgress[PINNED_MODEL_ID];
  const downloadFailed =
    !modelDownloaded && !modelDownloading && downloadError !== null;

  const [micAccess, setMicAccess] = useState<PermissionAccess | null>(null);
  const [micChecking, setMicChecking] = useState(isWindows);
  // The automatic download is attempted at most once per mount; after a
  // failure the retry button (not an effect) is the only way back in, so a
  // dead connection can't turn the panel into a retry loop.
  const autoTriggered = useRef(false);

  const checkMicPermission = useCallback(async () => {
    if (!isWindows) return;
    setMicChecking(true);
    try {
      // Returns the status directly: the registry probe has no error path.
      const status = await commands.getWindowsMicrophonePermissionStatus();
      setMicAccess(status.overall_access);
    } catch (e) {
      console.warn("Failed to check microphone permission status:", e);
    } finally {
      setMicChecking(false);
    }
  }, [isWindows]);

  useEffect(() => {
    if (isWindows) {
      void checkMicPermission();
    }
  }, [isWindows, checkMicPermission]);

  // Belt-and-braces: the backend auto-downloads the pinned model at startup.
  // If the store has settled and nothing is downloading (a backend race, or a
  // failed first attempt), make sure the download is running.
  useEffect(() => {
    if (modelsLoading || !parakeet) return;
    if (modelDownloaded || modelDownloading) return;
    if (autoTriggered.current) return;
    autoTriggered.current = true;
    void downloadModel(PINNED_MODEL_ID);
  }, [
    modelsLoading,
    parakeet,
    modelDownloaded,
    modelDownloading,
    downloadModel,
  ]);

  // Setup is done once the pinned model is on disk; App switches on the same
  // condition, so this only guards a re-render race.
  if (modelDownloaded) {
    return null;
  }

  const progressData =
    modelDownloading || modelVerifying
      ? [
          {
            id: PINNED_MODEL_ID,
            percentage: progress?.percentage ?? 0,
            speed:
              progress?.bytes_per_sec != null
                ? (progress.bytes_per_sec ?? 0) / 1_000_000
                : undefined,
          },
        ]
      : [];

  const micStatusLine = () => {
    if (!isWindows) {
      return t("firstRun.micWillPrompt");
    }
    if (micChecking || micAccess === null) {
      return <Spinner className="w-4 h-4" />;
    }
    if (micAccess === "denied") {
      return t("firstRun.micDenied");
    }
    if (micAccess === "allowed") {
      return t("firstRun.micReady");
    }
    return t("firstRun.micWillPrompt");
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="px-4 pt-2 space-y-1">
        <h1 className="text-lg font-medium text-text">{t("firstRun.title")}</h1>
        <p className="text-sm text-ink-2">{t("firstRun.subtitle")}</p>
      </div>

      {isWindows && (
        <SettingsGroup title={t("firstRun.micTitle")}>
          <div className="px-4 py-3 flex flex-col gap-3">
            <div className="flex items-center gap-2 text-sm">
              {micAccess === "denied" ? (
                <CircleAlert
                  width={16}
                  height={16}
                  className="shrink-0 text-logo-primary"
                />
              ) : (
                micAccess === "allowed" && (
                  <Check
                    width={16}
                    height={16}
                    className="shrink-0 text-logo-primary"
                  />
                )
              )}
              <span className="text-ink-2">{micStatusLine()}</span>
            </div>
            {micAccess === "denied" && (
              <div className="flex items-center gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    void commands.openMicrophonePrivacySettings();
                  }}
                >
                  {t("firstRun.micOpenSettings")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    void checkMicPermission();
                  }}
                >
                  <RotateCw className="w-4 h-4" />
                  {t("firstRun.micCheckAgain")}
                </Button>
              </div>
            )}
          </div>
        </SettingsGroup>
      )}

      <SettingsGroup title={t("firstRun.modelTitle")}>
        <div className="px-4 py-3 flex flex-col gap-3">
          {modelDownloaded ? (
            <div className="flex items-center gap-2 text-sm text-ink-2">
              <Check
                width={16}
                height={16}
                className="shrink-0 text-logo-primary"
              />
              {t("firstRun.modelReady")}
            </div>
          ) : (
            <>
              <p className="text-sm text-ink-2">
                {t("firstRun.modelDescription")}
              </p>
              {modelDownloading || modelVerifying ? (
                <ProgressBar
                  progress={progressData}
                  size="large"
                  showSpeed={true}
                />
              ) : downloadFailed ? (
                <div className="flex flex-col gap-2">
                  <div className="flex items-center gap-2 text-sm text-text">
                    <CircleAlert
                      width={16}
                      height={16}
                      className="shrink-0 text-logo-primary"
                    />
                    {t("firstRun.downloadFailed")}
                  </div>
                  <Button
                    variant="secondary"
                    size="sm"
                    className="w-fit"
                    onClick={() => {
                      void downloadModel(PINNED_MODEL_ID);
                    }}
                  >
                    <Download className="w-4 h-4" />
                    {t("firstRun.retryDownload")}
                  </Button>
                </div>
              ) : (
                <div className="flex items-center gap-2 text-sm text-ink-2">
                  <Spinner className="w-4 h-4" />
                  {t("downloads.downloading")}
                </div>
              )}
            </>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};

export default FirstRunSetup;
