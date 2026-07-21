using UUAV;

using System;
using TMPro;
using UnityEngine;
using UnityEngine.UI;
using UnityEngine.Assertions;

namespace UUAV.Example
{
    public class UIExamplePlayer : MonoBehaviour
    {
        [SerializeField] private RawImage surface;
        [SerializeField] private Button openButton;
        [SerializeField] private Button playButton;
        [SerializeField] private Button pauseButton;
        [SerializeField] private Button closeButton;
        [SerializeField] private TMP_InputField urlInput;
        [SerializeField] private TMP_Text statusText;
 

        [Header("Debug")]
        [SerializeField] private UUAVPlayer player;

        private void Awake()
        {
            Assert.IsNotNull(surface);
            Assert.IsNotNull(openButton);
            Assert.IsNotNull(playButton);
            Assert.IsNotNull(pauseButton);
            Assert.IsNotNull(closeButton);
            Assert.IsNotNull(urlInput);
            Assert.IsNotNull(statusText);

            player = UUAVPlayer.New();

            openButton.onClick.AddListener(() => player.OpenMedia(urlInput.text));
            playButton.onClick.AddListener(() => player.Play());
            pauseButton.onClick.AddListener(() => player.Pause());
            closeButton.onClick.AddListener(() => player.CloseMedia());
        }

        private void Update()
        {
            surface.texture = player.CurrentTexture;
            statusText.text = player.State.ToStringNoAlloc();
        }
    }
}

