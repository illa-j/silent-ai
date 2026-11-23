function apply(window) {
    if (!window) {
        return;
    }
    const caption = (window.caption || "").toString();
    const cls = (window.resourceClass || "").toString().toLowerCase();
    if (caption.indexOf("SILENT AI") >= 0 || caption.indexOf("Silent AI") >= 0 ||
        cls.indexOf("silent-ai") >= 0 || cls.indexOf("silent_ai") >= 0) {
        window.excludeFromCapture = true;
        console.info("Silent AI: excludeFromCapture=true for " + caption + " [" + cls + "]");
    }
}
workspace.windowAdded.connect(apply);
workspace.windowList().forEach(apply);
