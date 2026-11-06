// Bundled as text by the "rules" in wrangler.jsonc.
declare module "*.html" {
  const content: string;
  export default content;
}
declare module "*/openapi.json" {
  const content: string;
  export default content;
}
