/** @type {import('next').NextConfig} */
const nextConfig = {
    reactStrictMode: true,
    // Allow binary proxy streaming
    experimental: {
        serverActions: {
            bodySizeLimit: '50mb',
        },
    },
};

module.exports = nextConfig;
