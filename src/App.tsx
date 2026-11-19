import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import "./App.css";
import SecureInputWarning from "./components/SecureInputWarning";
import Footer from "./components/footer";
import { DebugSettings } from "./components/settings";
import { FirstRunSetup } from "./components/settings/FirstRunSetup";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { FileDropOverlay } from "./components/FileDropOverlay";
import { Sidebar, SidebarSection, SECTIONS_CONFIG } from "./components/Sidebar";
import { useSettings } from "./hooks/useSettings";
import { useSettingsStore } from "./stores/settingsStore";
import { useModelStore } from "./stores/modelStore";
import { commands } from "@/bindings";
import { NoticePanel } from "@/components/notices/NoticePanel";
import { PINNED_MODEL_ID } from "@/lib/pinnedModel";
import { getLanguageDirection, initializeRTL } from "@/lib/utils/rtl";

const renderSettingsContent = (section: SidebarSection) => {
  if (section === "debug") {
    return <DebugSettings />;
  }

  const ActiveComponent =
    SECTIONS_CONFIG[section]?.component || SECTIONS_CONFIG.general.component;
  return <ActiveComponent />;
};

function App() {
  const { i18n } = useTranslation();
  const [currentSection, setCurrentSection] =
    useState<SidebarSection>("general");
  const showHistory = useCallback(() => setCurrentSection("history"), []);
  const { settings, updateSetting } = useSettings();
  const direction = getLanguageDirection(i18n.language);
  const models = useModelStore((state) => state.models);
  const modelsLoading = useModelStore((state) => state.loading);
  // First run lasts exactly as long as the pinned Parakeet download.
  const parakeet = models.find((model) => model.id === PINNED_MODEL_ID);
  const firstRun =
    !modelsLoading && parakeet !== undefined && !parakeet.is_downloaded;
  const refreshAudioDevices = useSettingsStore(
    (state) => state.refreshAudioDevices,
  );
  const refreshOutputDevices = useSettingsStore(
    (state) => state.refreshOutputDevices,
  );
  const hasCompletedInit = useRef(false);
  const settingsScrollRef = useRef<HTMLDivElement>(null);
  // Reset the scroll position whenever the active section changes.
  useLayoutEffect(() => {
    settingsScrollRef.current?.scrollTo({ top: 0 });
  }, [currentSection]);

  // Initialize RTL direction when language changes
  useEffect(() => {
    initializeRTL(i18n.language);
  }, [i18n.language]);

  // Initialize Enigo, shortcuts, and refresh audio devices when main app loads
  useEffect(() => {
    if (settings !== null && !hasCompletedInit.current) {
      hasCompletedInit.current = true;
      Promise.all([
        commands.initializeEnigo(),
        commands.initializeShortcuts(),
      ]).catch((e) => {
        console.warn("Failed to initialize:", e);
      });
      refreshAudioDevices();
      refreshOutputDevices();
    }
  }, [settings, refreshAudioDevices, refreshOutputDevices]);

  // Mark first-run completion so the backend stops treating the profile as
  // new; Voicey has no tour — the settings window is the first-run surface.
  useEffect(() => {
    if (settings && settings.onboarding_completed !== true) {
      commands.completeOnboarding().catch((e) => {
        console.warn("Failed to complete onboarding:", e);
      });
    }
  }, [settings]);

  // Handle keyboard shortcuts for debug mode toggle
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      // Check for Ctrl+Shift+D (Windows/Linux) or Cmd+Shift+D (macOS)
      const isDebugShortcut =
        event.shiftKey &&
        event.key.toLowerCase() === "d" &&
        (event.ctrlKey || event.metaKey);

      if (isDebugShortcut) {
        event.preventDefault();
        const currentDebugMode = settings?.debug_mode ?? false;
        updateSetting("debug_mode", !currentDebugMode);
      }
    };

    // Add event listener when component mounts
    document.addEventListener("keydown", handleKeyDown);

    // Cleanup function when component unmounts
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [settings?.debug_mode, updateSetting]);

  // Wait for settings to load before rendering the app shell.
  if (settings === null) {
    return null;
  }

  const content: ReactNode = (
    <div
      dir={direction}
      className="h-screen flex flex-col select-none cursor-default"
    >
      {/* Main content area that takes remaining space */}
      <div className="flex-1 flex overflow-hidden">
        <Sidebar
          activeSection={currentSection}
          onSectionChange={setCurrentSection}
        />
        {/* Scrollable content area */}
        <div className="flex-1 flex flex-col overflow-hidden">
          <div ref={settingsScrollRef} className="flex-1 overflow-y-auto">
            <div className="flex flex-col items-center p-4 gap-4">
              <SecureInputWarning />
              {firstRun ? (
                <FirstRunSetup />
              ) : (
                renderSettingsContent(currentSection)
              )}
            </div>
          </div>
        </div>
      </div>
      {/* Fixed footer at bottom */}
      <Footer onShowHistory={showHistory} />
      <FileDropOverlay onDropped={showHistory} />
    </div>
  );

  return (
    <>
      {content}
      <NoticePanel />
    </>
  );
}

export default App;
