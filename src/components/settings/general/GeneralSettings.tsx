import React from "react";
import { useTranslation } from "react-i18next";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { ShortcutInput } from "../ShortcutInput";
import { SettingsGroup } from "../../ui/SettingsGroup";

/**
 * The whole settings surface (D8): a keyboard shortcut and a microphone.
 * Everything else Voicey does is a settled default — pinned Parakeet model,
 * VAD on, dictation as-is, minimal overlay, paste with trailing space,
 * no auto-submit, dark theme — and has no UI.
 */
export const GeneralSettings: React.FC = () => {
  const { t } = useTranslation();
  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.general.title")}>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
        <MicrophoneSelector descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
    </div>
  );
};
