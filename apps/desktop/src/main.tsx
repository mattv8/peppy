import { StrictMode } from "react"; import { createRoot } from "react-dom/client"; import { App } from "./App"; import { InvalidMediaViewerWindow, isMediaViewerRoute, MediaViewerWindow, readMediaViewerWindow } from "./media/MediaViewerWindow"; import "./app.css";
export function DesktopRoot({ search = location.search }: { search?: string }) {
  const mediaViewerWindow = readMediaViewerWindow(search);
  return mediaViewerWindow ? <MediaViewerWindow {...mediaViewerWindow} /> : isMediaViewerRoute(search) ? <InvalidMediaViewerWindow /> : <App />;
}
createRoot(document.getElementById("root")!).render(<StrictMode><DesktopRoot /></StrictMode>);
