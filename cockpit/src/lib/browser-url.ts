/** Browser entry point for a platform app; public HTTPS keeps its public route. */
export function browserUrl(url: string, userspace: boolean): string {
  if (!userspace) return url;
  try {
    const u = new URL(url);
    if (u.hostname.endsWith(".localhost")) return url;
    const mesh = u.hostname.endsWith(".vfy") || /(?:^|[.-])10[.-]77[.-]\d+[.-]\d+(?:\.(?:sslip|nip)\.io)?$/.test(u.hostname);
    if (u.protocol !== "http:" && !(u.protocol === "https:" && mesh)) return url;
    u.protocol = "http:";
    u.hostname += ".localhost";
    u.port = "7481";
    return u.href;
  } catch {
    return url;
  }
}
