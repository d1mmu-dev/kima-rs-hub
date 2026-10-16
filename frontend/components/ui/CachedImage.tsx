"use client";

import Image, { ImageProps } from "next/image";
import { memo, useState } from "react";

interface CachedImageProps extends Omit<ImageProps, "src"> {
    src: string | null | undefined;
    fill?: boolean;
}

/* Cover art is served cache-first by the service worker for /api/library/cover-art/*. */
const CachedImage = memo(function CachedImage({
    src,
    alt = "",
    ...props
}: CachedImageProps) {
    const [failedSrc, setFailedSrc] = useState<string | null>(null);

    if (!src || failedSrc === src) {
        return null;
    }

    return (
        <Image
            src={src}
            alt={alt}
            unoptimized
            {...props}
            loading={props.loading ?? (props.priority || props.preload ? undefined : "lazy")}
            onError={(e) => {
                setFailedSrc(src);
                props.onError?.(e);
            }}
        />
    );
});

export { CachedImage };
