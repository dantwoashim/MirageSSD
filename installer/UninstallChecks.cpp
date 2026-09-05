// The uninstall guard is implemented by `mirage-service.exe --uninstall-check` and scheduled
// before StopServices in Product.wxs. Keeping the check in the service binary prevents a second
// state parser from drifting from the durable control-plane schema.
