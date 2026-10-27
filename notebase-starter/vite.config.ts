import { defineConfig } from 'vite';
export default defineConfig({server:{port:1420,strictPort:true,watch:{ignored:['**/target/**','**/test-results/**','**/playwright-report/**']}},clearScreen:false});
