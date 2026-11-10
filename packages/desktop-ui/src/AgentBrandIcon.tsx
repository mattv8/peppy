import { AGENT_ICON_GEOMETRY } from "./agent-icons/paths";
import type { AgentBrandId } from "./agent-icons/brands";

export type { AgentBrandId } from "./agent-icons/brands";

export type AgentBrandIconProps = {
  clientId: AgentBrandId;
  className?: string;
  size?: number;
};

export function AgentBrandIcon({ clientId, className, size = 16 }: AgentBrandIconProps) {
  const geometry = AGENT_ICON_GEOMETRY[clientId];
  return (
    <svg
      className={className}
      viewBox={geometry.viewBox}
      width={size}
      height={size}
      fill="currentColor"
      fillRule={geometry.fillRule}
      aria-hidden="true"
      focusable="false"
      data-agent-icon={clientId}
    >
      {geometry.paths.map((path) => (
        <path key={path.d} d={path.d} clipRule={path.clipRule} />
      ))}
    </svg>
  );
}
