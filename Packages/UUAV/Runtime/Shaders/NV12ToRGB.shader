Shader "Hidden/UUAV/NV12ToRGB"
{
    Properties
    {
        _YTex ("Y plane", 2D) = "black" {}
        _UVTex ("UV plane", 2D) = "gray" {}
    }
    SubShader
    {
        Pass
        {
            ZTest Always
            ZWrite Off
            Cull Off

            CGPROGRAM
            #pragma vertex vert_img
            #pragma fragment frag
            #include "UnityCG.cginc"

            sampler2D _YTex;
            sampler2D _UVTex;

            fixed4 frag(v2f_img i) : SV_Target
            {
                // video rows are top-down, Unity UVs are bottom-up
                float2 uv = float2(i.uv.x, 1.0 - i.uv.y);

                // BT.709 limited range
                // TODO select BT.601 / full range once native exports colorimetry
                float y = 1.1643835 * (tex2D(_YTex, uv).r - 0.0627451);
                float2 c = tex2D(_UVTex, uv).rg - 0.5; // r = Cb, g = Cr
                float3 rgb = float3(
                    y + 1.7927411 * c.y,
                    y - 0.2132486 * c.x - 0.5329093 * c.y,
                    y + 2.1124018 * c.x);
                return fixed4(saturate(rgb), 1.0);
            }
            ENDCG
        }
    }
    Fallback Off
}
