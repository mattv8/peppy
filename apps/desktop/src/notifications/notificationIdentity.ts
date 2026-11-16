export const notificationTuple = (...parts: string[]) =>
  encodeURIComponent(JSON.stringify(parts));

export const notificationMuteKey = (sourceDeviceId: string, packageName: string) =>
  notificationTuple(sourceDeviceId, packageName);
