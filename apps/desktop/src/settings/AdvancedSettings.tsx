import { WebhookSettings } from "./WebhookSettings";

type AdvancedSettingsProps = Parameters<typeof WebhookSettings>[0];

export function AdvancedSettings(props: AdvancedSettingsProps) {
  return <section id="settings-advanced" className="settings-card" data-settings-section="advanced"><h2>Advanced</h2><WebhookSettings {...props} /></section>;
}
