import { resolve } from 'path';
import { cpSync, existsSync } from 'node:fs';
import { defineConfig } from 'vite';

export default defineConfig({
    build: {
        rollupOptions: {
            input: {
                script: resolve(__dirname, 'crates/arc-app/src/resources/js/script.js'),
                styles: resolve(__dirname, 'crates/arc-app/src/resources/css/styles.css'),
            },
            output: {
                entryFileNames: '[name]-[hash].js',
                chunkFileNames: '[name]-[hash].js',
                assetFileNames: '[name]-[hash][extname]',
            },
        },
        outDir: 'dist',
        emptyOutDir: true,
        cssCodeSplit: true,
        manifest: true,
    },
    plugins: [
        {
            name: 'arc-copy-static',
            writeBundle() {
                const source = resolve(__dirname, 'crates/arc-app/src/resources/imgs');
                if (existsSync(source)) cpSync(source, resolve(__dirname, 'dist/imgs'), { recursive: true });
                // Shared Workbench rail script, the same file `arc new --ui` generates.
                cpSync(resolve(__dirname, 'crates/arc-cli/templates/public/workbench.js'), resolve(__dirname, 'dist/workbench.js'));
            },
        },
    ],
});
