module f (
    input  logic clk,
    input  logic v__valid [0:2],
    input  logic [7:0] v__val [0:2],
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] shifted [0:3];
    for (genvar i = 0; i < 4; i++) begin : g_i
        assign shifted[i] = (a * 2);
    end
    logic [7:0] picked [0:2];
    logic whole__valid [0:2];
    logic [7:0] whole__val [0:2];
    for (genvar i_1 = 0; i_1 < 3; i_1++) begin : g_x
        logic x__valid;
        logic [7:0] x__val;
        assign x__valid = v__valid[i_1];
        assign x__val = v__val[i_1];
        logic [7:0] __block_0;
        always_comb begin
            if (x__valid) begin
                __block_0 = x__val;
            end else begin
                __block_0 = 0;
            end
        end
        assign picked[i_1] = __block_0;
        assign whole__valid[i_1] = x__valid;
        assign whole__val[i_1] = x__val;
    end
    logic [7:0] m [0:2];
    assign m[0] = a;
    assign m[1] = b;
    assign m[2] = (a + b);
    assign result = (((shifted[0] + picked[1]) + whole__val[2]) + m[0]);
endmodule
