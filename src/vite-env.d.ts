declare module "*.png" {
  const assetUrl: string;
  export default assetUrl;
}

interface ImportMetaEnv {
  readonly VITE_PUBLIC_APP_URL: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}