type VoiceyWordmarkProps = {
  className?: string;
};

/** The Voicey wordmark: a single red recording dot beside the product name. */
const VoiceyWordmark = ({ className = "" }: VoiceyWordmarkProps) => (
  <span className={`inline-flex items-center gap-2 ${className}`}>
    <span
      className="w-2 h-2 rounded-full bg-background-ui shrink-0"
      style={{ backgroundColor: "#e5484d" }}
      aria-hidden="true"
    />
    {/* eslint-disable-next-line i18next/no-literal-string */}
    <span className="text-[15px] font-semibold tracking-tight text-text">
      Voicey
    </span>
  </span>
);

export default VoiceyWordmark;
