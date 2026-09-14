using System;
using System.Globalization;
using System.IO;
using System.Numerics;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Svg;
using Windows.Foundation;
using Windows.Storage.Streams;
using Windows.UI;

namespace Looker.Imaging;

/// <summary>
/// Rasterizes SVG via Win2D's <see cref="CanvasSvgDocument"/> into a <see cref="CanvasRenderTarget"/>
/// at the requested size. Vector re-rasterization on zoom (crisp at any scale) is a later refinement;
/// for now it renders at the fit target. Returns null if SVG isn't supported so the router falls
/// through to Magick.
/// </summary>
public sealed class SvgDecoder : IImageDecoder
{
    public bool CanDecode(ImageFormat format) => format == ImageFormat.Svg;

    public async Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        CancellationToken ct = request.Cancellation;
        if (!CanvasSvgDocument.IsSupported(device))
            return null;

        using FileStream fs = File.OpenRead(request.Path);
        using IRandomAccessStream ras = fs.AsRandomAccessStream();
        CanvasSvgDocument svg = await CanvasSvgDocument.LoadAsync(device, ras).AsTask(ct);

        try
        {
            Size intrinsic = ReadIntrinsicSize(svg);
            int box = Math.Max(request.TargetWidth, request.TargetHeight);
            if (box <= 0)
                box = 1024;

            double baseW = intrinsic.Width > 0 ? intrinsic.Width : box;
            double baseH = intrinsic.Height > 0 ? intrinsic.Height : box;
            double scale = Math.Min(box / baseW, box / baseH);
            if (scale <= 0 || double.IsInfinity(scale) || double.IsNaN(scale))
                scale = 1.0;

            int w = Math.Max(1, (int)Math.Round(baseW * scale));
            int h = Math.Max(1, (int)Math.Round(baseH * scale));

            ct.ThrowIfCancellationRequested();
            var target = new CanvasRenderTarget(device, w, h, 96);
            using (CanvasDrawingSession ds = target.CreateDrawingSession())
            {
                ds.Clear(Color.FromArgb(0, 0, 0, 0));
                // DrawSvg's viewport does not scale a document with an absolute width/height (it renders at
                // intrinsic size and crops to the target), so scale the drawing session instead.
                ds.Transform = Matrix3x2.CreateScale((float)scale);
                ds.DrawSvg(svg, new Size(baseW, baseH));
            }

            return new DecodedImage
            {
                Bitmap = target,
                OrientedNativeSize = new Size(baseW, baseH),
                IsFullResolution = false,
                Format = ImageFormat.Svg,
            };
        }
        finally
        {
            svg.Dispose();
        }
    }

    /// <summary>Best-effort intrinsic size from the root width/height or viewBox; (0,0) if unknown.</summary>
    internal static Size ReadIntrinsicSize(CanvasSvgDocument svg)
    {
        try
        {
            CanvasSvgNamedElement root = svg.Root;

            double w = TryReadLength(root, "width");
            double h = TryReadLength(root, "height");
            if (w > 0 && h > 0)
                return new Size(w, h);

            string viewBox = TryReadString(root, "viewBox");
            if (!string.IsNullOrWhiteSpace(viewBox))
            {
                string[] parts = viewBox.Split(new[] { ' ', ',', '\t' }, StringSplitOptions.RemoveEmptyEntries);
                if (parts.Length == 4 &&
                    double.TryParse(parts[2], NumberStyles.Float, CultureInfo.InvariantCulture, out double vw) &&
                    double.TryParse(parts[3], NumberStyles.Float, CultureInfo.InvariantCulture, out double vh) &&
                    vw > 0 && vh > 0)
                {
                    return new Size(vw, vh);
                }
            }
        }
        catch
        {
            // Fall back to a square render box.
        }

        return new Size(0, 0);
    }

    private static double TryReadLength(CanvasSvgNamedElement element, string name)
    {
        try
        {
            if (element.IsAttributeSpecified(name))
                return element.GetLengthAttribute(name, out _);
        }
        catch
        {
            // ignore
        }
        return 0;
    }

    private static string TryReadString(CanvasSvgNamedElement element, string name)
    {
        try
        {
            if (element.IsAttributeSpecified(name))
                return element.GetStringAttribute(name);
        }
        catch
        {
            // ignore
        }
        return string.Empty;
    }
}
