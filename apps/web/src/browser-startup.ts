import type { HostedWebConfig } from "./newhosted-app-auth";

type CallbackResult = "none" | "complete" | "retry";

export type BrowserStartupDependencies = {
  fetchConfig: () => Promise<HostedWebConfig | undefined>;
  exchangeCallback: (setCsrf: (csrf: string) => Promise<void>) => Promise<CallbackResult>;
  setCsrf: (csrf: string) => Promise<void>;
  retryAuthorization: (config: HostedWebConfig) => Promise<boolean>;
  mount: (config: HostedWebConfig | undefined) => void;
  isActive: () => boolean;
};

export async function startBrowserApplication(dependencies: BrowserStartupDependencies): Promise<void> {
  const config = await dependencies.fetchConfig();
  if (!dependencies.isActive()) return;
  if (!config) return dependencies.mount(undefined);

  let result: CallbackResult;
  try {
    result = await dependencies.exchangeCallback(dependencies.setCsrf);
  } catch {
    if (dependencies.isActive()) dependencies.mount(config);
    return;
  }
  if (!dependencies.isActive()) return;
  if (result !== "complete") await dependencies.retryAuthorization(config);
  if (dependencies.isActive()) dependencies.mount(config);
}
