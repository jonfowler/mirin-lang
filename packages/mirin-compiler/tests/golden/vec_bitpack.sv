module pack_vec (
    input  logic clk,
    input  logic [7:0] v [0:2],
    output logic [23:0] result
);
    Vec__BitPack__pack__uint8 #(
        .N(3)
    ) Vec__BitPack__pack__uint8 (
        .self(v),
        .result(result)
    );
endmodule

module unpack_vec (
    input  logic clk,
    input  logic [23:0] b,
    output logic [7:0] result [0:2]
);
    Vec__BitPack__unpack__uint8 #(
        .N(3)
    ) Vec__BitPack__unpack__uint8 (
        .b(b),
        .result(result)
    );
endmodule

module Vec__BitPack__pack__uint8 #(parameter int N) (
    input  logic [7:0] self [0:N-1],
    output logic [(N * 8)-1:0] result
);
    logic [(N * 8)-1:0] b;
    for (genvar i = 0; i < N; i++) begin : g_i
        logic [7:0] pelem;
        assign pelem = (self[i]);
        for (genvar j = 0; j < 8; j++) begin : g_j
            assign b[((i * 8) + j)] = pelem[j];
        end
    end
    assign result = b;
endmodule

module Vec__BitPack__unpack__uint8 #(parameter int N) (
    input  logic [(N * 8)-1:0] b,
    output logic [7:0] result [0:N-1]
);
    logic [7:0] acc [0:N-1];
    for (genvar i = 0; i < N; i++) begin : g_i
        logic [7:0] eb;
        for (genvar j = 0; j < 8; j++) begin : g_j
            assign eb[j] = b[((i * 8) + j)];
        end
        assign acc[i] = (eb);
    end
    assign result = acc;
endmodule
