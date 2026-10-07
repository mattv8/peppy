import { BrowserWorkerHost, browserHostDependencies, type BrowserPort } from "./host.js";

const worker = self as unknown as SharedWorkerGlobalScope;
const host = new BrowserWorkerHost(browserHostDependencies(worker.navigator.locks, () => worker.close()));

worker.onconnect = event => { void host.attach(event.ports[0] as BrowserPort); };
